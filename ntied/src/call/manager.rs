use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::anyhow;
use ntied_transport::Address;
use tokio::sync::{Mutex as TokioMutex, RwLock};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::audio::AudioManager;
use crate::contact::{ContactHandle, ContactManager};
use crate::packet::{
    AudioDataPacket, CallAcceptPacket, CallEndPacket, CallPacket, CallRejectPacket,
    CallStartPacket, VideoDataPacket,
};

use super::{CallHandle, CallListener, CallState, StubListener};

pub struct CallManager {
    contact_manager: Arc<ContactManager>,
    active_calls: Arc<RwLock<HashMap<Address, CallHandle>>>,
    current_call: Arc<RwLock<Option<CallHandle>>>,
    listener: Arc<dyn CallListener>,
    polling_tasks: Arc<TokioMutex<HashMap<Address, JoinHandle<()>>>>,
    audio_manager: Arc<AudioManager>,
    audio_capture_task: Arc<TokioMutex<Option<JoinHandle<()>>>>,
}

impl CallManager {
    pub fn new(contact_manager: Arc<ContactManager>) -> Arc<Self> {
        Self::with_listener(contact_manager, Arc::new(StubListener))
    }

    pub fn with_listener<L>(contact_manager: Arc<ContactManager>, listener: Arc<L>) -> Arc<Self>
    where
        L: CallListener + 'static,
    {
        let audio_manager = Arc::new(AudioManager::new().expect("Failed to create audio manager"));

        let manager = Arc::new(Self {
            contact_manager,
            active_calls: Arc::new(RwLock::new(HashMap::new())),
            current_call: Arc::new(RwLock::new(None)),
            listener,
            polling_tasks: Arc::new(TokioMutex::new(HashMap::new())),
            audio_manager,
            audio_capture_task: Arc::new(TokioMutex::new(None)),
        });

        // Start main polling coordinator task
        let manager_clone = manager.clone();
        tokio::spawn(manager_clone.manage_polling_tasks());

        manager
    }

    pub async fn start_call(
        &self,
        address: Address,
        video_enabled: bool,
    ) -> Result<CallHandle, anyhow::Error> {
        tracing::info!(
            "Starting call to address: {}, video: {}",
            address,
            video_enabled
        );

        // Check if already in a call
        let current = self.current_call.read().await;
        if current.is_some() {
            tracing::warn!("Cannot start call - already in a call");
            return Err(anyhow!("Already in a call"));
        }
        drop(current);

        // Get contact handle
        let contact_handle = self.contact_manager.connect_contact(address).await;
        if !contact_handle.is_connected() {
            tracing::error!("Cannot start call - contact {} is not connected", address);
            return Err(anyhow!("Contact is not connected"));
        }
        tracing::debug!("Contact {} is connected, proceeding with call", address);

        // Create call handle
        let call_id = Uuid::now_v7();
        let call_handle = CallHandle::new(
            call_id,
            address,
            false, // outgoing
            video_enabled,
            contact_handle.clone(),
            self.listener.clone(),
        );

        // Store call handle
        let mut calls = self.active_calls.write().await;
        calls.insert(address, call_handle.clone());
        drop(calls);

        let mut current = self.current_call.write().await;
        *current = Some(call_handle.clone());
        drop(current);

        // Send call start packet
        let packet = CallPacket::Start(CallStartPacket {
            call_id,
            video_enabled,
        });

        tracing::debug!("Sending call start packet with call_id: {}", call_id);
        contact_handle.send_call_packet(packet).await.map_err(|e| {
            tracing::error!("Failed to send call start packet: {}", e);
            anyhow!("Failed to send call start packet: {}", e)
        })?;

        call_handle.set_state(CallState::Calling).await;
        tracing::info!(
            "Call started successfully to {}, call_id: {}",
            address,
            call_id
        );

        // Notify listener with video flag
        self.listener.on_outgoing_call(address, video_enabled).await;

        Ok(call_handle)
    }

    async fn handle_incoming_call(
        &self,
        address: Address,
        packet: CallStartPacket,
    ) -> Result<(), anyhow::Error> {
        tracing::info!(
            "Received incoming call from {}, call_id: {}, video: {}",
            address,
            packet.call_id,
            packet.video_enabled
        );

        // Check if already in a call
        let current = self.current_call.read().await;
        if let Some(existing_call) = current.as_ref() {
            let state = existing_call.get_state().await;
            if state != CallState::Idle && state != CallState::Ended {
                tracing::warn!(
                    "Rejecting incoming call from {} - already in a call",
                    address
                );
                // We're busy - reject the call
                self.reject_incoming_call(address, packet.call_id).await?;
                return Ok(());
            }
        }
        drop(current);

        // Get contact handle
        let contact_handle = self.contact_manager.connect_contact(address).await;

        // Create call handle for incoming call
        let call_handle = CallHandle::new(
            packet.call_id,
            address,
            true, // incoming
            packet.video_enabled,
            contact_handle,
            self.listener.clone(),
        );

        // Store call handle
        let mut calls = self.active_calls.write().await;
        calls.insert(address, call_handle.clone());
        drop(calls);

        call_handle.set_state(CallState::Ringing).await;
        tracing::info!("Incoming call ready from {}, state set to Ringing", address);

        // Notify listener
        self.listener
            .on_incoming_call(address, packet.video_enabled)
            .await;

        Ok(())
    }

    pub async fn accept_call(&self, address: Address) -> Result<(), anyhow::Error> {
        tracing::info!("Accepting call from {}", address);

        let calls = self.active_calls.read().await;
        let call_handle = calls
            .get(&address)
            .ok_or_else(|| {
                tracing::error!("No incoming call found from {}", address);
                anyhow!("No incoming call from address")
            })?
            .clone();
        drop(calls);

        let state = call_handle.get_state().await;
        if state != CallState::Ringing {
            return Err(anyhow!("Call is not in ringing state"));
        }

        let contact_handle = call_handle.contact_handle();

        // Send accept packet
        let packet = CallPacket::Accept(CallAcceptPacket {
            call_id: call_handle.call_id(),
            video_enabled: call_handle.is_video_enabled(),
        });

        contact_handle
            .send_call_packet(packet)
            .await
            .map_err(|e| anyhow!("Failed to send accept packet: {}", e))?;

        // Update state
        call_handle.set_state(CallState::Connected).await;

        // Set as current call
        let mut current = self.current_call.write().await;
        *current = Some(call_handle.clone());
        drop(current);

        tracing::info!("Call accepted from {}, starting audio", address);

        // Start audio capture and playback
        if let Err(e) = self.start_audio_for_call().await {
            tracing::error!("Failed to start audio for call: {}", e);
        } else {
            tracing::info!("Audio started successfully for call");
        }

        // Notify listener
        self.listener.on_call_accepted(address).await;
        self.listener.on_call_connected(address).await;

        Ok(())
    }

    pub async fn reject_call(&self, address: Address) -> Result<(), anyhow::Error> {
        let calls = self.active_calls.read().await;
        let call_handle = calls
            .get(&address)
            .ok_or_else(|| anyhow!("No call from address"))?
            .clone();
        drop(calls);

        let contact_handle = call_handle.contact_handle();

        // Send reject packet
        let packet = CallPacket::Reject(CallRejectPacket {
            call_id: call_handle.call_id(),
        });

        contact_handle
            .send_call_packet(packet)
            .await
            .map_err(|e| anyhow!("Failed to send reject packet: {}", e))?;

        // Clean up
        call_handle.set_state(CallState::Ended).await;
        self.cleanup_call(address).await;

        // Notify listener
        self.listener.on_call_rejected(address).await;

        Ok(())
    }

    async fn reject_incoming_call(
        &self,
        address: Address,
        call_id: Uuid,
    ) -> Result<(), anyhow::Error> {
        let contact_handle = self.contact_manager.connect_contact(address).await;

        let packet = CallPacket::Reject(CallRejectPacket { call_id });

        contact_handle
            .send_call_packet(packet)
            .await
            .map_err(|e| anyhow!("Failed to send reject packet: {}", e))?;

        Ok(())
    }

    pub async fn end_call(&self, address: Address) -> Result<(), anyhow::Error> {
        let calls = self.active_calls.read().await;
        let call_handle = calls
            .get(&address)
            .ok_or_else(|| anyhow!("No active call with address"))?
            .clone();
        drop(calls);

        let contact_handle = call_handle.contact_handle();

        // Send end packet
        let packet = CallPacket::End(CallEndPacket {
            call_id: call_handle.call_id(),
        });

        contact_handle
            .send_call_packet(packet)
            .await
            .map_err(|e| anyhow!("Failed to send end packet: {}", e))?;

        // Clean up
        call_handle.set_state(CallState::Ended).await;
        self.cleanup_call(address).await;

        // Notify listener
        self.listener
            .on_call_ended(address, "User ended call")
            .await;

        Ok(())
    }

    async fn handle_call_accepted(
        &self,
        address: Address,
        packet: CallAcceptPacket,
    ) -> Result<(), anyhow::Error> {
        let calls = self.active_calls.read().await;
        let call_handle = calls
            .get(&address)
            .ok_or_else(|| anyhow!("No call with address"))?
            .clone();
        drop(calls);

        // Verify call ID
        if call_handle.call_id() != packet.call_id {
            return Err(anyhow!("Call ID mismatch"));
        }

        call_handle.set_state(CallState::Connected).await;

        // Set as current call
        let mut current = self.current_call.write().await;
        *current = Some(call_handle.clone());
        drop(current);

        // Start audio capture and playback
        if let Err(e) = self.start_audio_for_call().await {
            tracing::error!("Failed to start audio for call: {}", e);
        }

        // Notify listener
        self.listener.on_call_connected(address).await;

        Ok(())
    }

    async fn handle_call_rejected(
        &self,
        address: Address,
        packet: CallRejectPacket,
    ) -> Result<(), anyhow::Error> {
        let calls = self.active_calls.read().await;
        let call_handle = calls.get(&address).cloned();
        drop(calls);

        if let Some(handle) = call_handle {
            if handle.call_id() == packet.call_id {
                handle.set_state(CallState::Ended).await;
                self.cleanup_call(address).await;
                self.listener.on_call_ended(address, "Call rejected").await;
            }
        }

        Ok(())
    }

    async fn handle_call_ended(
        &self,
        address: Address,
        packet: CallEndPacket,
    ) -> Result<(), anyhow::Error> {
        let calls = self.active_calls.read().await;
        let call_handle = calls.get(&address).cloned();
        drop(calls);

        if let Some(handle) = call_handle {
            if handle.call_id() == packet.call_id {
                handle.set_state(CallState::Ended).await;
                self.cleanup_call(address).await;
                self.listener
                    .on_call_ended(address, "Remote ended call")
                    .await;
            }
        }

        Ok(())
    }

    async fn handle_audio_data(
        &self,
        address: Address,
        packet: AudioDataPacket,
    ) -> Result<(), anyhow::Error> {
        let calls = self.active_calls.read().await;
        let call_handle = calls.get(&address).cloned();
        drop(calls);

        if let Some(handle) = call_handle {
            if handle.call_id() == packet.call_id {
                tracing::trace!(
                    "Received audio packet from {}, {} bytes",
                    address,
                    packet.data.len()
                );

                // Play received audio through audio manager
                // Only log errors at trace level during device switching to avoid spam
                if let Err(e) = self.audio_manager.play_audio(packet.data.clone()).await {
                    // Check if we're in the middle of switching devices
                    if self.audio_manager.is_playing() {
                        tracing::error!("Failed to play audio: {}", e);
                    } else {
                        tracing::trace!(
                            "Audio playback not available (possibly switching devices): {}",
                            e
                        );
                    }
                }

                // Also notify listener
                self.listener
                    .on_audio_data_received(address, packet.data)
                    .await;
            }
        }

        Ok(())
    }

    async fn handle_video_frame(
        &self,
        address: Address,
        packet: VideoDataPacket,
    ) -> Result<(), anyhow::Error> {
        let calls = self.active_calls.read().await;
        let call_handle = calls.get(&address).cloned();
        drop(calls);

        if let Some(handle) = call_handle {
            if handle.call_id() == packet.call_id {
                // Pass video frame to listener for display
                self.listener
                    .on_video_frame_received(address, packet.frame)
                    .await;
            }
        }

        Ok(())
    }

    async fn cleanup_call(&self, address: Address) {
        // Stop audio capture and playback
        if let Err(e) = self.stop_audio().await {
            tracing::error!("Failed to stop audio: {}", e);
        }

        let mut calls = self.active_calls.write().await;
        calls.remove(&address);
        drop(calls);

        let mut current = self.current_call.write().await;
        if let Some(call) = current.as_ref() {
            if call.peer_address() == address {
                *current = None;
            }
        }
    }

    pub async fn get_current_call(&self) -> Option<CallHandle> {
        self.current_call.read().await.clone()
    }

    pub async fn is_in_call(&self) -> bool {
        let current = self.current_call.read().await;
        if let Some(call) = current.as_ref() {
            let state = call.get_state().await;
            state != CallState::Idle && state != CallState::Ended
        } else {
            false
        }
    }

    pub async fn toggle_mute(&self) -> Result<bool, anyhow::Error> {
        let current = self.current_call.read().await;
        let call_handle = current.as_ref().ok_or_else(|| anyhow!("No active call"))?;
        let is_muted = call_handle.toggle_mute().await?;
        tracing::info!("Microphone {}", if is_muted { "muted" } else { "unmuted" });
        Ok(is_muted)
    }

    pub async fn toggle_video(&self) -> Result<bool, anyhow::Error> {
        let current = self.current_call.read().await;
        let call_handle = current.as_ref().ok_or_else(|| anyhow!("No active call"))?;
        call_handle.toggle_video().await
    }

    pub async fn get_current_input_device(&self) -> Option<String> {
        // Return the currently active input device name if there's an active call
        let current = self.current_call.read().await;
        if current.is_some() && self.audio_manager.is_capturing() {
            self.audio_manager.get_current_input_device().await
        } else {
            None
        }
    }

    pub async fn get_current_output_device(&self) -> Option<String> {
        // Return the currently active output device name if there's an active call
        let current = self.current_call.read().await;
        if current.is_some() && self.audio_manager.is_playing() {
            self.audio_manager.get_current_output_device().await
        } else {
            None
        }
    }

    pub async fn switch_input_device(
        &self,
        device_name: Option<String>,
    ) -> Result<(), anyhow::Error> {
        // Check if there's an active call
        let current = self.current_call.read().await;
        if current.is_none() {
            return Err(anyhow!("No active call"));
        }
        drop(current);

        // Stop current capture
        if let Some(task) = self.audio_capture_task.lock().await.take() {
            task.abort();
        }
        self.audio_manager.stop_capture().await?;

        // Small delay to ensure clean stop
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // Start capture with new device
        let device_name_clone = device_name.clone();
        let mut audio_rx = self.audio_manager.start_capture(device_name_clone).await?;

        // Restart capture task
        let current = self.current_call.read().await;
        if let Some(call_handle) = current.as_ref() {
            let call_handle = call_handle.clone();
            let task = tokio::spawn(async move {
                while let Some(data) = audio_rx.recv().await {
                    // If muted, send silence (zeros) instead of actual audio
                    let audio_data = if call_handle.is_muted() {
                        tracing::trace!("Microphone muted, sending silence");
                        vec![0u8; data.len()] // Send silence of same length
                    } else {
                        data
                    };

                    let data_len = audio_data.len();
                    // Create audio packet
                    let packet = CallPacket::AudioData(AudioDataPacket {
                        call_id: call_handle.call_id(),
                        timestamp: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_millis() as u64,
                        data: audio_data,
                    });

                    // Send through contact handle
                    if let Err(e) = call_handle.contact_handle().send_call_packet(packet).await {
                        tracing::error!("Failed to send captured audio: {}", e);
                        break;
                    } else {
                        tracing::trace!("Sent audio packet, {} bytes", data_len);
                    }
                }
            });

            let mut capture_task = self.audio_capture_task.lock().await;
            *capture_task = Some(task);
        }
        drop(current);

        Ok(())
    }

    pub async fn switch_output_device(
        &self,
        device_name: Option<String>,
    ) -> Result<(), anyhow::Error> {
        // Check if there's an active call
        let current = self.current_call.read().await;
        if current.is_none() {
            return Err(anyhow!("No active call"));
        }
        drop(current);

        // Switch playback device with minimal interruption
        // Store the new device name for tracking
        let device_name_clone = device_name.clone();

        // Stop current playback
        self.audio_manager.stop_playback().await?;

        // Small delay to ensure clean stop
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // Start playback with new device
        self.audio_manager.start_playback(device_name_clone).await?;

        Ok(())
    }

    async fn start_audio_for_call(&self) -> Result<(), anyhow::Error> {
        tracing::debug!("Starting audio subsystems for call");

        // Start audio playback
        self.audio_manager.start_playback(None).await?;
        tracing::debug!("Audio playback started");

        // Start audio capture
        let mut audio_rx = self.audio_manager.start_capture(None).await?;
        tracing::debug!("Audio capture started");

        // Start task to send captured audio
        // We need to get a handle to send audio through the current call
        let current = self.current_call.read().await;
        if let Some(call_handle) = current.as_ref() {
            let call_handle = call_handle.clone();
            let task = tokio::spawn(async move {
                while let Some(data) = audio_rx.recv().await {
                    // If muted, send silence (zeros) instead of actual audio
                    let audio_data = if call_handle.is_muted() {
                        tracing::trace!("Microphone muted, sending silence");
                        vec![0u8; data.len()] // Send silence of same length
                    } else {
                        data
                    };

                    let data_len = audio_data.len();
                    let packet = CallPacket::AudioData(AudioDataPacket {
                        call_id: call_handle.call_id(),
                        timestamp: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_millis() as u64,
                        data: audio_data,
                    });

                    // Send through contact handle
                    if let Err(e) = call_handle.contact_handle().send_call_packet(packet).await {
                        tracing::error!("Failed to send captured audio: {}", e);
                        break;
                    } else {
                        tracing::trace!("Sent audio packet, {} bytes", data_len);
                    }
                }
            });

            let mut capture_task = self.audio_capture_task.lock().await;
            *capture_task = Some(task);
        }
        drop(current);

        Ok(())
    }

    async fn stop_audio(&self) -> Result<(), anyhow::Error> {
        // Stop capture task
        if let Some(task) = self.audio_capture_task.lock().await.take() {
            task.abort();
        }

        // Stop audio capture
        self.audio_manager.stop_capture().await?;

        // Stop audio playback
        self.audio_manager.stop_playback().await?;

        Ok(())
    }

    async fn manage_polling_tasks(self: Arc<Self>) {
        // Check contacts every second to start/stop polling tasks
        let mut interval = tokio::time::interval(Duration::from_secs(1));

        loop {
            interval.tick().await;

            // Get current contacts
            let contacts = self.contact_manager.list_contacts().await;
            let mut tasks = self.polling_tasks.lock().await;

            // Remove tasks for disconnected contacts
            let mut to_remove = Vec::new();
            for address in tasks.keys() {
                if !contacts
                    .iter()
                    .any(|c| c.address() == *address && c.is_connected())
                {
                    to_remove.push(*address);
                }
            }

            for address in to_remove {
                if let Some(task) = tasks.remove(&address) {
                    tracing::debug!("Stopping call packet polling for {}", address);
                    task.abort();
                }
            }

            // Start tasks for new connected contacts
            for contact_handle in contacts {
                if !contact_handle.is_connected() {
                    continue;
                }

                let address = contact_handle.address();
                if !tasks.contains_key(&address) {
                    // Start a dedicated polling task for this contact
                    let manager = self.clone();
                    let contact = contact_handle.clone();
                    let task = tokio::spawn(async move {
                        manager.poll_contact_packets(address, contact).await;
                    });
                    tasks.insert(address, task);
                    tracing::debug!("Started call packet polling for {}", address);
                }
            }
        }
    }

    async fn poll_contact_packets(
        self: Arc<Self>,
        address: Address,
        contact_handle: ContactHandle,
    ) {
        // Poll this specific contact continuously for call packets
        loop {
            // Check if still connected
            if !contact_handle.is_connected() {
                tracing::debug!("Contact {} disconnected, stopping polling", address);
                break;
            }

            // Try to receive call packets with very short timeout
            let recv_future = contact_handle.recv_call_packet();
            let timeout_result =
                tokio::time::timeout(Duration::from_millis(100), recv_future).await;

            match timeout_result {
                Ok(Ok(packet)) => {
                    // Don't log audio packets at debug level to avoid spam
                    match &packet {
                        CallPacket::AudioData(_) => {
                            tracing::trace!("Received audio packet from {}", address);
                        }
                        _ => {
                            tracing::debug!("Received call packet from {}: {:?}", address, packet);
                        }
                    }

                    // Process the received packet
                    if let Err(e) = self.process_call_packet(address, packet).await {
                        tracing::error!("Failed to process call packet from {}: {}", address, e);
                    }
                }
                Ok(Err(e)) => {
                    tracing::error!("Error receiving call packet from {}: {}", address, e);
                    break;
                }
                Err(_) => {
                    // Timeout - normal, just continue
                }
            }

            // Small yield to prevent tight loop
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }

    async fn process_call_packet(
        &self,
        address: Address,
        packet: CallPacket,
    ) -> Result<(), anyhow::Error> {
        match packet {
            CallPacket::Start(p) => self.handle_incoming_call(address, p).await,
            CallPacket::Accept(p) => self.handle_call_accepted(address, p).await,
            CallPacket::Reject(p) => self.handle_call_rejected(address, p).await,
            CallPacket::End(p) => self.handle_call_ended(address, p).await,
            CallPacket::AudioData(p) => self.handle_audio_data(address, p).await,
            CallPacket::VideoData(p) => self.handle_video_frame(address, p).await,
        }
    }
}

impl Drop for CallManager {
    fn drop(&mut self) {
        // Cancel all polling tasks when manager is dropped
        let polling_tasks = self.polling_tasks.clone();
        let audio_capture_task = self.audio_capture_task.clone();
        let audio_manager = self.audio_manager.clone();

        tokio::spawn(async move {
            let mut tasks = polling_tasks.lock().await;
            for (_, task) in tasks.drain() {
                task.abort();
            }
            if let Some(task) = audio_capture_task.lock().await.take() {
                task.abort();
            }
            // Stop audio
            let _ = audio_manager.stop_capture().await;
            let _ = audio_manager.stop_playback().await;
        });
    }
}
