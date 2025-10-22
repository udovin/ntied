use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::anyhow;
use ntied_transport::Address;
use tokio::sync::{Mutex as TokioMutex, RwLock};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::audio::{AudioManager, CodecManager, CodecType, NegotiatedCodec};
use crate::contact::{ContactHandle, ContactManager};
use crate::packet::{
    AudioDataPacket, CallAcceptPacket, CallEndPacket, CallPacket, CallRejectPacket,
    CallStartPacket, CodecAnswerPacket, CodecOfferPacket, VideoDataPacket,
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
    codec_manager: Arc<CodecManager>,
}

impl CallManager {
    pub fn new(contact_manager: Arc<ContactManager>) -> Arc<Self> {
        Self::with_listener(contact_manager, Arc::new(StubListener))
    }

    pub fn with_listener<L>(contact_manager: Arc<ContactManager>, listener: Arc<L>) -> Arc<Self>
    where
        L: CallListener + 'static,
    {
        let audio_manager = Arc::new(AudioManager::new());
        let codec_manager = Arc::new(CodecManager::new());

        let manager = Arc::new(Self {
            contact_manager,
            active_calls: Arc::new(RwLock::new(HashMap::new())),
            current_call: Arc::new(RwLock::new(None)),
            listener,
            polling_tasks: Arc::new(TokioMutex::new(HashMap::new())),
            audio_manager,
            audio_capture_task: Arc::new(TokioMutex::new(None)),
            codec_manager,
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

        // Send codec offer
        let codec_offer = self.codec_manager.create_offer();
        let offer_packet = CallPacket::CodecOffer(CodecOfferPacket {
            call_id,
            capabilities: self.codec_manager.capabilities().clone(),
            preferred_codec: codec_offer.clone(),
        });

        tracing::debug!(
            "Sending codec offer with preferred codec: {:?}",
            codec_offer.codec
        );
        contact_handle
            .send_call_packet(offer_packet)
            .await
            .map_err(|e| {
                tracing::error!("Failed to send codec offer: {}", e);
                anyhow!("Failed to send codec offer: {}", e)
            })?;

        // Initialize codec with our offer (will reinitialize if answer differs)
        if let Err(e) = self.codec_manager.initialize(&codec_offer).await {
            tracing::warn!("Failed to initialize codec with offer: {}", e);
        }

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

        // Note: Codec answer will be sent when we receive CodecOffer

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

    async fn handle_codec_offer(
        &self,
        address: Address,
        packet: CodecOfferPacket,
    ) -> Result<(), anyhow::Error> {
        tracing::info!(
            "Received codec offer from {}, preferred: {:?}",
            address,
            packet.preferred_codec.codec
        );

        let calls = self.active_calls.read().await;
        let call_handle = calls.get(&address).cloned();
        drop(calls);

        if let Some(handle) = call_handle {
            if handle.call_id() == packet.call_id {
                // Create answer based on remote capabilities
                let answer = match self.codec_manager.create_answer(&packet.capabilities) {
                    Ok(answer) => answer,
                    Err(e) => {
                        tracing::error!("Failed to create codec answer: {}", e);
                        // Fall back to raw codec
                        NegotiatedCodec {
                            codec: CodecType::Raw,
                            params: crate::audio::CodecParams::default(),
                            is_offerer: false,
                        }
                    }
                };

                // Send codec answer
                let answer_packet = CallPacket::CodecAnswer(CodecAnswerPacket {
                    call_id: packet.call_id,
                    negotiated_codec: answer.clone(),
                });

                handle
                    .contact_handle()
                    .send_call_packet(answer_packet)
                    .await
                    .map_err(|e| anyhow!("Failed to send codec answer: {}", e))?;

                // Initialize codec with negotiated settings
                if let Err(e) = self.codec_manager.initialize(&answer).await {
                    tracing::error!("Failed to initialize codec: {}", e);
                }

                tracing::info!("Codec negotiation complete, using: {:?}", answer.codec);
            }
        }

        Ok(())
    }

    async fn handle_codec_answer(
        &self,
        address: Address,
        packet: CodecAnswerPacket,
    ) -> Result<(), anyhow::Error> {
        tracing::info!(
            "Received codec answer from {}, negotiated: {:?}",
            address,
            packet.negotiated_codec.codec
        );

        let calls = self.active_calls.read().await;
        let call_handle = calls.get(&address).cloned();
        drop(calls);

        if let Some(handle) = call_handle {
            if handle.call_id() == packet.call_id {
                // Reinitialize codec with the negotiated settings
                if let Err(e) = self
                    .codec_manager
                    .initialize(&packet.negotiated_codec)
                    .await
                {
                    tracing::error!("Failed to initialize negotiated codec: {}", e);
                }

                tracing::info!(
                    "Codec initialized with negotiated settings: {:?} at {} Hz",
                    packet.negotiated_codec.codec,
                    packet.negotiated_codec.params.sample_rate
                );
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
                // Validate and normalize sample rate
                let normalized_sample_rate = match packet.sample_rate {
                    // Common sample rates
                    8000 | 16000 | 24000 | 32000 | 44100 | 48000 | 96000 => packet.sample_rate,
                    // Handle unusual rates
                    rate if rate > 0 && rate < 8000 => {
                        tracing::warn!(
                            "Very low sample rate {} Hz from {}, using 8000 Hz",
                            rate,
                            address
                        );
                        8000
                    }
                    rate if rate > 96000 => {
                        tracing::warn!(
                            "Very high sample rate {} Hz from {}, using 48000 Hz",
                            rate,
                            address
                        );
                        48000
                    }
                    0 => {
                        tracing::error!("Invalid sample rate 0 from {}, using 48000 Hz", address);
                        48000
                    }
                    rate => {
                        tracing::info!(
                            "Non-standard sample rate {} Hz from {}, using as-is",
                            rate,
                            address
                        );
                        rate
                    }
                };

                tracing::debug!(
                    "Received audio packet from {}, sequence {}, codec {:?}, {} bytes, sample_rate: {} Hz (normalized: {} Hz), channels: {}",
                    address,
                    packet.sequence,
                    packet.codec,
                    packet.data.len(),
                    packet.sample_rate,
                    normalized_sample_rate,
                    packet.channels
                );

                // Already validated sample rate above
                if packet.channels == 0 || packet.channels > 8 {
                    tracing::warn!(
                        "Unusual channel count received: {} from {}",
                        packet.channels,
                        address
                    );
                }

                // Decode the audio data
                // IMPORTANT: The decoder returns samples at the codec's configured sample rate,
                // NOT necessarily the sample_rate field from the packet.
                // We must get the actual decoder output rate from codec params.
                let decoded_samples =
                    match self.codec_manager.decode(packet.codec, &packet.data).await {
                        Ok(samples) => {
                            tracing::trace!(
                                "Successfully decoded {} samples from {} byte packet",
                                samples.len(),
                                packet.data.len()
                            );
                            samples
                        }
                        Err(e) => {
                            tracing::warn!(
                                "Failed to decode audio packet (sequence {}, codec {:?}): {}",
                                packet.sequence,
                                packet.codec,
                                e
                            );
                            // Try packet loss concealment
                            match self.codec_manager.conceal_packet_loss().await {
                                Ok(samples) => {
                                    tracing::debug!(
                                        "Applied packet loss concealment for sequence {}",
                                        packet.sequence
                                    );
                                    samples
                                }
                                Err(plc_err) => {
                                    // If PLC fails, skip this packet
                                    tracing::warn!("Packet loss concealment failed: {}", plc_err);
                                    return Ok(());
                                }
                            }
                        }
                    };

                // Get the actual sample rate from the decoder's params
                // This is the REAL sample rate of decoded_samples, not the packet's claimed rate
                // The decoder always outputs at its configured sample rate, regardless of what
                // the packet claims. Using the wrong rate here causes pitch shifting!
                let actual_sample_rate = match self.codec_manager.decoder_output_sample_rate().await
                {
                    Some(rate) => {
                        tracing::trace!(
                            "Using decoder output rate: {} Hz (packet claimed: {} Hz)",
                            rate,
                            packet.sample_rate
                        );
                        rate
                    }
                    None => {
                        // Fallback: if decoder is not initialized, use normalized packet rate
                        tracing::warn!(
                            "Decoder not initialized, falling back to packet sample rate: {} Hz",
                            normalized_sample_rate
                        );
                        normalized_sample_rate
                    }
                };

                // Store the length before moving decoded_samples
                let decoded_samples_len = decoded_samples.len();

                // Convert decoded samples back to bytes for listener compatibility (temporary)
                // This can be removed once listener is updated
                let bytes: Vec<u8> = decoded_samples
                    .iter()
                    .flat_map(|&sample| {
                        let sample_i16 = (sample.max(-1.0).min(1.0) * 32767.0) as i16;
                        sample_i16.to_le_bytes()
                    })
                    .collect();

                // Queue audio frame for playback with jitter buffer
                // CRITICAL: Use actual_sample_rate which is the real sample rate of decoded samples
                if let Err(e) = self
                    .audio_manager
                    .queue_audio_frame(
                        packet.sequence,
                        decoded_samples,
                        actual_sample_rate, // Use ACTUAL decoder output rate, not packet rate
                        packet.channels,
                    )
                    .await
                {
                    if self.audio_manager.is_playing().await {
                        tracing::error!(
                            "Failed to queue audio frame (sequence {}, {} samples @ {}Hz): {}",
                            packet.sequence,
                            decoded_samples_len,
                            actual_sample_rate,
                            e
                        );
                    } else {
                        tracing::trace!(
                            "Audio playback not available (possibly switching devices): {}",
                            e
                        );
                    }
                } else {
                    tracing::trace!(
                        "Queued audio frame: sequence {}, {} samples @ {}Hz",
                        packet.sequence,
                        decoded_samples_len,
                        actual_sample_rate
                    );
                }

                // Notify listener
                self.listener.on_audio_data_received(address, bytes).await;
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
        if current.is_some() && self.audio_manager.is_capturing().await {
            self.audio_manager.get_current_input_device().await
        } else {
            None
        }
    }

    pub async fn get_current_output_device(&self) -> Option<String> {
        // Return the currently active output device name if there's an active call
        let current = self.current_call.read().await;
        if current.is_some() && self.audio_manager.is_playing().await {
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
        // Stop existing capture and task
        if let Some(task) = self.audio_capture_task.lock().await.take() {
            task.abort();
        }
        self.audio_manager.stop_capture().await?;

        // Small delay to ensure clean stop
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // Start capture with new device
        let mut audio_rx = self.audio_manager.start_capture(device_name, 1.0).await?;

        // Start task to send captured audio
        let current = self.current_call.read().await;
        if let Some(call_handle) = current.as_ref() {
            let call_handle = call_handle.clone();
            let audio_manager = self.audio_manager.clone();
            let codec_manager = self.codec_manager.clone();
            let task = tokio::spawn(async move {
                while let Some(frame) = audio_rx.recv().await {
                    // If muted, send silence instead of actual audio
                    let samples = if call_handle.is_muted() {
                        tracing::trace!("Microphone muted, sending silence");
                        vec![0.0f32; frame.samples.len()]
                    } else {
                        frame.samples.clone()
                    };

                    // Encode the audio samples
                    let (codec_type, encoded_data) = match codec_manager.encode(&samples).await {
                        Ok(result) => result,
                        Err(e) => {
                            tracing::error!("Failed to encode audio: {}", e);
                            // Fall back to raw if encoding fails
                            (
                                CodecType::Raw,
                                samples.iter().flat_map(|s| s.to_le_bytes()).collect(),
                            )
                        }
                    };

                    // Prepare frame with sequence number
                    let (sequence, _) = audio_manager.prepare_audio_frame(frame.clone()).await;

                    let packet = CallPacket::AudioData(AudioDataPacket {
                        call_id: call_handle.call_id(),
                        sequence,
                        timestamp: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_millis() as u64,
                        codec: codec_type,
                        data: encoded_data,
                        sample_rate: frame.sample_rate,
                        channels: frame.channels,
                    });

                    // Send through contact handle
                    if let Err(e) = call_handle.contact_handle().send_call_packet(packet).await {
                        tracing::error!("Failed to send captured audio: {}", e);
                        break;
                    } else {
                        tracing::trace!("Sent audio packet, sequence {}", sequence);
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
        // Stop current playback
        self.audio_manager.stop_playback().await?;

        // Small delay to ensure clean stop
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // Restart playback with new device (use default volume 1.0)
        self.audio_manager.start_playback(device_name, 1.0).await?;

        Ok(())
    }

    async fn start_audio_for_call(&self) -> Result<(), anyhow::Error> {
        tracing::debug!("Starting audio subsystems for call");

        // Reset sequence counter for new call
        self.audio_manager.reset_sequence();

        // Start audio playback
        self.audio_manager.start_playback(None, 1.0).await?;
        tracing::debug!("Audio playback started");

        // Start audio capture
        let mut audio_rx = self.audio_manager.start_capture(None, 1.0).await?;
        tracing::debug!("Audio capture started");

        // Start task to send captured audio
        // We need to get a handle to send audio through the current call
        let audio_manager = self.audio_manager.clone();
        let codec_manager = self.codec_manager.clone();
        let current = self.current_call.read().await;
        if let Some(call_handle) = current.as_ref() {
            let call_handle_clone = call_handle.clone();
            let audio_manager_clone = audio_manager.clone();
            let codec_manager_clone = codec_manager.clone();
            let task = tokio::spawn(async move {
                while let Some(frame) = audio_rx.recv().await {
                    // If muted, send silence instead of actual audio
                    let samples = if call_handle_clone.is_muted() {
                        tracing::trace!("Microphone muted, sending silence");
                        vec![0.0f32; frame.samples.len()]
                    } else {
                        frame.samples.clone()
                    };

                    // Validate captured sample rate
                    let valid_sample_rate = match frame.sample_rate {
                        8000 | 16000 | 24000 | 32000 | 44100 | 48000 | 96000 => frame.sample_rate,
                        rate => {
                            tracing::warn!(
                                "Capture device using non-standard rate {} Hz, continuing anyway",
                                rate
                            );
                            rate
                        }
                    };

                    // Log frame info for debugging
                    tracing::trace!(
                        "Captured audio frame: {} samples, {} Hz, {} channels, RMS: {:.4}",
                        frame.samples.len(),
                        valid_sample_rate,
                        frame.channels,
                        if frame.samples.is_empty() {
                            0.0
                        } else {
                            let sum: f32 = frame.samples.iter().map(|s| s * s).sum();
                            (sum / frame.samples.len() as f32).sqrt()
                        }
                    );

                    // Prepare frame with sequence number
                    // Encode the audio samples
                    let (codec_type, encoded_data) =
                        match codec_manager_clone.encode(&samples).await {
                            Ok(result) => result,
                            Err(e) => {
                                tracing::error!(
                                    "Failed to encode audio ({} samples): {}",
                                    samples.len(),
                                    e
                                );
                                // Fall back to raw if encoding fails
                                (
                                    CodecType::Raw,
                                    samples.iter().flat_map(|s| s.to_le_bytes()).collect(),
                                )
                            }
                        };

                    let (sequence, _) =
                        audio_manager_clone.prepare_audio_frame(frame.clone()).await;

                    // Store the length before moving encoded_data
                    let encoded_data_len = encoded_data.len();

                    tracing::trace!(
                        "Sending audio packet: sequence {}, codec {:?}, {} bytes, sample_rate: {} Hz, channels: {}",
                        sequence,
                        codec_type,
                        encoded_data_len,
                        valid_sample_rate,
                        frame.channels
                    );

                    let packet = CallPacket::AudioData(AudioDataPacket {
                        call_id: call_handle_clone.call_id(),
                        sequence,
                        timestamp: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_millis() as u64,
                        codec: codec_type,
                        data: encoded_data,
                        sample_rate: valid_sample_rate,
                        channels: frame.channels,
                    });

                    // Send through contact handle
                    if let Err(e) = call_handle_clone
                        .contact_handle()
                        .send_call_packet(packet)
                        .await
                    {
                        tracing::error!("Failed to send captured audio: {}", e);
                        break;
                    } else {
                        tracing::trace!("Sent audio packet, sequence {}", sequence);
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
            CallPacket::CodecOffer(p) => self.handle_codec_offer(address, p).await,
            CallPacket::CodecAnswer(p) => self.handle_codec_answer(address, p).await,
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
