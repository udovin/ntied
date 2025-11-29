use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::anyhow;
use ntied_transport::Address;
use tokio::sync::{Mutex as TokioMutex, RwLock};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::audio::{
    AudioConfig, AudioManager, CaptureStream, CodecManager, CodecType, Decoder, Encoder,
    PlaybackStream,
};
use crate::contact::{ContactHandle, ContactManager};
use crate::packet::{
    AudioDataPacket, CallAcceptPacket, CallEndPacket, CallPacket, CallRejectPacket,
    CallStartPacket, CodecAnswerPacket, CodecOfferPacket, VideoDataPacket,
};

use super::{CallHandle, CallListener, CallState, StubListener};

/// Audio state for the active call - only one can exist at a time
struct AudioState {
    decoder: Arc<Decoder>,
    capture_stream: Arc<TokioMutex<CaptureStream>>,
    playback_stream: Arc<TokioMutex<PlaybackStream>>,
    capture_task: JoinHandle<()>,
    playback_task: JoinHandle<()>,
    encoder_task: JoinHandle<()>,
    input_device_name: Option<String>,
    output_device_name: Option<String>,
    codec_type: CodecType,
}

impl Drop for AudioState {
    fn drop(&mut self) {
        self.capture_task.abort();
        self.playback_task.abort();
        self.encoder_task.abort();
        tracing::debug!("Audio state dropped - all tasks aborted");
    }
}

pub struct CallManager {
    contact_manager: Arc<ContactManager>,
    active_calls: Arc<RwLock<HashMap<Address, CallHandle>>>,
    current_call: Arc<RwLock<Option<CallHandle>>>,
    listener: Arc<dyn CallListener>,
    polling_tasks: Arc<TokioMutex<HashMap<Address, JoinHandle<()>>>>,
    audio_state: Arc<TokioMutex<Option<AudioState>>>,
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
        let codec_manager = Arc::new(CodecManager::new());

        let manager = Arc::new(Self {
            contact_manager,
            active_calls: Arc::new(RwLock::new(HashMap::new())),
            current_call: Arc::new(RwLock::new(None)),
            listener,
            polling_tasks: Arc::new(TokioMutex::new(HashMap::new())),
            audio_state: Arc::new(TokioMutex::new(None)),
            codec_manager,
        });

        // Start main polling coordinator task
        let manager_clone = manager.clone();
        tokio::spawn(manager_clone.manage_polling_tasks());

        manager
    }

    pub async fn start_call(&self, address: Address) -> Result<CallHandle, anyhow::Error> {
        tracing::info!("Starting call to address: {}", address);

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
        let packet = CallPacket::Start(CallStartPacket { call_id });

        tracing::debug!("Sending call start packet with call_id: {}", call_id);
        contact_handle.send_call_packet(packet).await.map_err(|e| {
            tracing::error!("Failed to send call start packet: {}", e);
            anyhow!("Failed to send call start packet: {}", e)
        })?;

        // Send codec offer
        let codec_offer = self.codec_manager.create_offer();
        let offer_packet = CallPacket::CodecOffer(CodecOfferPacket {
            call_id,
            capabilities: self.codec_manager.capabilities().await,
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

        call_handle.set_state(CallState::Calling).await;
        tracing::info!(
            "Call started successfully to {}, call_id: {}",
            address,
            call_id
        );

        // Notify listener with video flag
        self.listener.on_outgoing_call(address).await;

        Ok(call_handle)
    }

    async fn handle_incoming_call(
        &self,
        address: Address,
        packet: CallStartPacket,
    ) -> Result<(), anyhow::Error> {
        tracing::info!(
            "Received incoming call from {}, call_id: {}",
            address,
            packet.call_id,
        );

        // Check if already in a call
        let current = self.current_call.read().await;
        if let Some(existing_call) = current.as_ref() {
            let state = existing_call.get_state().await;
            if state != CallState::Idle && state != CallState::Ended {
                tracing::warn!(
                    "Already in a call with state {:?}, rejecting incoming call from {}",
                    state,
                    address
                );
                drop(current);
                self.reject_incoming_call(address, packet.call_id).await?;
                return Ok(());
            }
        }
        drop(current);

        // Get or create contact handle
        let contact_handle = self.contact_manager.connect_contact(address).await;
        if !contact_handle.is_connected() {
            tracing::error!(
                "Cannot accept incoming call - contact {} is not connected",
                address
            );
            return Err(anyhow!("Contact is not connected"));
        }

        // Create call handle
        let call_handle = CallHandle::new(
            packet.call_id,
            address,
            true, // incoming
            contact_handle.clone(),
            self.listener.clone(),
        );

        // Store as active call
        let mut calls = self.active_calls.write().await;
        calls.insert(address, call_handle.clone());
        drop(calls);

        let mut current = self.current_call.write().await;
        *current = Some(call_handle.clone());
        drop(current);

        call_handle.set_state(CallState::Ringing).await;

        // Notify listener
        self.listener.on_incoming_call(address).await;

        Ok(())
    }

    pub async fn accept_call(&self, address: Address) -> Result<(), anyhow::Error> {
        tracing::info!("Accepting call from {}", address);

        let current = self.current_call.read().await;
        let call_handle = current.as_ref().ok_or_else(|| anyhow!("No current call"))?;

        if call_handle.peer_address() != address {
            return Err(anyhow!("Current call is not from {}", address));
        }

        let state = call_handle.get_state().await;
        if state != CallState::Ringing {
            return Err(anyhow!("Call is not in ringing state: {:?}", state));
        }

        let call_id = call_handle.call_id();
        let contact_handle = call_handle.contact_handle().clone();

        drop(current);

        // Send accept packet
        let packet = CallPacket::Accept(CallAcceptPacket { call_id });
        contact_handle
            .send_call_packet(packet)
            .await
            .map_err(|e| anyhow!("Failed to send accept packet: {}", e))?;

        // Send codec offer
        let codec_offer = self.codec_manager.create_offer();
        let offer_packet = CallPacket::CodecOffer(CodecOfferPacket {
            call_id,
            capabilities: self.codec_manager.capabilities().await,
            preferred_codec: codec_offer.clone(),
        });

        contact_handle
            .send_call_packet(offer_packet)
            .await
            .map_err(|e| anyhow!("Failed to send codec offer: {}", e))?;

        // Start audio for this call
        if let Err(e) = self.start_audio_for_call().await {
            tracing::error!("Failed to start audio for call: {}", e);
        } else {
            tracing::debug!("Audio started successfully for accepted call");
        }

        let current = self.current_call.read().await;
        if let Some(call_handle) = current.as_ref() {
            call_handle.set_state(CallState::Connected).await;
        }
        drop(current);

        // Notify listener that call was accepted and is now connected
        self.listener.on_call_accepted(address).await;

        self.listener.on_call_connected(address).await;

        tracing::info!("Call accepted from {}", address);
        Ok(())
    }

    pub async fn reject_call(&self, address: Address) -> Result<(), anyhow::Error> {
        tracing::info!("Rejecting call from {}", address);

        let current = self.current_call.read().await;
        let call_handle = current.as_ref().ok_or_else(|| anyhow!("No current call"))?;

        if call_handle.peer_address() != address {
            return Err(anyhow!("Current call is not from {}", address));
        }

        let call_id = call_handle.call_id();
        let contact_handle = call_handle.contact_handle().clone();

        drop(current);

        // Send reject packet
        let packet = CallPacket::Reject(CallRejectPacket { call_id });
        contact_handle
            .send_call_packet(packet)
            .await
            .map_err(|e| anyhow!("Failed to send reject packet: {}", e))?;

        // Cleanup
        self.cleanup_call(address).await;

        // Notify listener
        self.listener.on_call_rejected(address).await;
        self.listener.on_call_ended(address, "Call rejected").await;

        tracing::info!("Call rejected from {}", address);
        Ok(())
    }

    async fn reject_incoming_call(
        &self,
        address: Address,
        call_id: Uuid,
    ) -> Result<(), anyhow::Error> {
        tracing::debug!("Rejecting incoming call from {} (busy)", address);

        let contact_handle = self.contact_manager.connect_contact(address).await;

        let packet = CallPacket::Reject(CallRejectPacket { call_id });
        contact_handle
            .send_call_packet(packet)
            .await
            .map_err(|e| anyhow!("Failed to send reject packet: {}", e))?;

        Ok(())
    }

    pub async fn end_call(&self, address: Address) -> Result<(), anyhow::Error> {
        tracing::info!("Ending call with {}", address);

        let current = self.current_call.read().await;
        let call_handle = current.as_ref().ok_or_else(|| anyhow!("No current call"))?;

        if call_handle.peer_address() != address {
            return Err(anyhow!("Current call is not with {}", address));
        }

        let call_id = call_handle.call_id();
        let contact_handle = call_handle.contact_handle().clone();

        drop(current);

        // Send end packet
        let packet = CallPacket::End(CallEndPacket { call_id });
        if let Err(e) = contact_handle.send_call_packet(packet).await {
            tracing::warn!("Failed to send end packet: {}", e);
        }

        // Cleanup
        self.cleanup_call(address).await;

        // Notify listener
        self.listener.on_call_ended(address, "Call ended").await;

        tracing::info!("Call ended with {}", address);
        Ok(())
    }

    async fn handle_call_accepted(
        &self,
        address: Address,
        _packet: CallAcceptPacket,
    ) -> Result<(), anyhow::Error> {
        tracing::info!("Call accepted by {}", address);

        let current = self.current_call.read().await;
        if let Some(call_handle) = current.as_ref() {
            if call_handle.peer_address() == address {
                call_handle.set_state(CallState::Connected).await;
                drop(current);

                // Start audio for this call
                if let Err(e) = self.start_audio_for_call().await {
                    tracing::error!("Failed to start audio for call: {}", e);
                }

                self.listener.on_call_connected(address).await;
            }
        }

        Ok(())
    }

    async fn handle_call_rejected(
        &self,
        address: Address,
        _packet: CallRejectPacket,
    ) -> Result<(), anyhow::Error> {
        tracing::info!("Call rejected by {}", address);

        self.cleanup_call(address).await;
        self.listener.on_call_rejected(address).await;
        self.listener.on_call_ended(address, "Call rejected").await;

        Ok(())
    }

    async fn handle_call_ended(
        &self,
        address: Address,
        _packet: CallEndPacket,
    ) -> Result<(), anyhow::Error> {
        tracing::info!("Call ended by {}", address);

        self.cleanup_call(address).await;
        self.listener
            .on_call_ended(address, "Remote ended call")
            .await;

        Ok(())
    }

    async fn handle_codec_offer(
        &self,
        address: Address,
        packet: CodecOfferPacket,
    ) -> Result<(), anyhow::Error> {
        tracing::debug!(
            "Received codec offer from {}: preferred={:?}",
            address,
            packet.preferred_codec.codec
        );

        // Check if this is for our current call
        let current = self.current_call.read().await;
        let call_handle = current.as_ref().ok_or_else(|| anyhow!("No current call"))?;

        if call_handle.peer_address() != address {
            tracing::warn!(
                "Received codec offer from {} but current call is with {}",
                address,
                call_handle.peer_address()
            );
            return Ok(());
        }

        let call_id = call_handle.call_id();
        let contact_handle = call_handle.contact_handle().clone();

        drop(current);

        // Create answer based on their capabilities
        let answer = self.codec_manager.create_answer(&packet.capabilities)?;

        // Send codec answer
        let answer_packet = CallPacket::CodecAnswer(CodecAnswerPacket {
            call_id,
            negotiated_codec: answer.clone(),
        });

        contact_handle
            .send_call_packet(answer_packet)
            .await
            .map_err(|e| anyhow!("Failed to send codec answer: {}", e))?;

        tracing::info!("Codec negotiation complete, using: {:?}", answer.codec);

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

        // Just log it - codec is already set when creating AudioState
        Ok(())
    }

    async fn handle_audio_data(
        &self,
        address: Address,
        packet: AudioDataPacket,
    ) -> Result<(), anyhow::Error> {
        // Check if this is for our current call
        let current = self.current_call.read().await;
        if let Some(call_handle) = current.as_ref() {
            if call_handle.peer_address() != address || call_handle.call_id() != packet.call_id {
                return Ok(());
            }
        } else {
            return Ok(());
        }
        drop(current);

        // Get audio state and send packet to decoder
        let audio = self.audio_state.lock().await;
        if let Some(state) = audio.as_ref() {
            // Send packet to decoder - it will handle decoding, jitter buffer, PLC, and playback
            tracing::trace!(
                "Received audio packet, size: {} bytes, forwarding to decoder",
                packet.data.len()
            );
            if let Err(e) = state.decoder.send_packet(packet).await {
                tracing::warn!("Failed to send packet to decoder: {}", e);
            }
        } else {
            tracing::warn!("Received audio packet but no audio state exists");
        }

        Ok(())
    }

    async fn handle_video_frame(
        &self,
        address: Address,
        packet: VideoDataPacket,
    ) -> Result<(), anyhow::Error> {
        // Check if this is for our current call
        let current = self.current_call.read().await;
        if let Some(call_handle) = current.as_ref() {
            if call_handle.peer_address() == address && call_handle.call_id() == packet.call_id {
                drop(current);
                // Pass video frame to listener for display
                self.listener
                    .on_video_frame_received(address, packet.frame)
                    .await;
            }
        }

        Ok(())
    }

    async fn cleanup_call(&self, address: Address) {
        // Set call state to Ended before cleanup
        let current = self.current_call.read().await;
        if let Some(call) = current.as_ref() {
            if call.peer_address() == address {
                call.set_state(CallState::Ended).await;
            }
        }
        let is_current_call = current
            .as_ref()
            .map(|c| c.peer_address() == address)
            .unwrap_or(false);
        drop(current);

        if is_current_call {
            let mut audio = self.audio_state.lock().await;
            if audio.take().is_some() {
                tracing::debug!("Audio state stopped for address {}", address);
            }
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

    pub async fn is_muted(&self) -> Result<bool, anyhow::Error> {
        let current = self.current_call.read().await;
        let call_handle = current.as_ref().ok_or_else(|| anyhow!("No active call"))?;
        Ok(call_handle.is_muted())
    }

    pub async fn toggle_mute(&self) -> Result<bool, anyhow::Error> {
        let current = self.current_call.read().await;
        let call_handle = current.as_ref().ok_or_else(|| anyhow!("No active call"))?;
        let is_muted = call_handle.toggle_mute().await?;
        tracing::info!("Microphone {}", if is_muted { "muted" } else { "unmuted" });
        Ok(is_muted)
    }

    pub async fn get_current_input_device(&self) -> Option<String> {
        let audio = self.audio_state.lock().await;
        audio.as_ref().and_then(|s| s.input_device_name.clone())
    }

    pub async fn get_current_output_device(&self) -> Option<String> {
        let audio = self.audio_state.lock().await;
        audio.as_ref().and_then(|s| s.output_device_name.clone())
    }

    pub async fn switch_input_device(
        &self,
        device_name: Option<String>,
    ) -> Result<(), anyhow::Error> {
        let current = self.current_call.read().await;
        let call_handle = current.as_ref().ok_or_else(|| anyhow!("No active call"))?;

        let call_id = call_handle.call_id();
        let contact_handle = call_handle.contact_handle().clone();
        let call_handle_clone = call_handle.clone();
        drop(current);

        let mut audio = self.audio_state.lock().await;
        let old_state = audio.take().ok_or_else(|| anyhow!("No audio state"))?;

        let output_device_name = old_state.output_device_name.clone();
        let codec_type = old_state.codec_type;

        drop(old_state);
        drop(audio);

        // Recreate audio with new input device
        tracing::info!("Switching input device to: {:?}", device_name);
        self.create_audio_state(
            call_id,
            codec_type,
            device_name,
            output_device_name,
            contact_handle,
            call_handle_clone,
        )
        .await?;

        Ok(())
    }

    pub async fn switch_output_device(
        &self,
        device_name: Option<String>,
    ) -> Result<(), anyhow::Error> {
        let current = self.current_call.read().await;
        let call_handle = current.as_ref().ok_or_else(|| anyhow!("No active call"))?;

        let call_id = call_handle.call_id();
        let contact_handle = call_handle.contact_handle().clone();
        let call_handle_clone = call_handle.clone();
        drop(current);

        let mut audio = self.audio_state.lock().await;
        let old_state = audio.take().ok_or_else(|| anyhow!("No audio state"))?;

        let input_device_name = old_state.input_device_name.clone();
        let codec_type = old_state.codec_type;

        drop(old_state);
        drop(audio);

        // Recreate audio with new output device
        tracing::info!("Switching output device to: {:?}", device_name);
        self.create_audio_state(
            call_id,
            codec_type,
            input_device_name,
            device_name,
            contact_handle,
            call_handle_clone,
        )
        .await?;

        Ok(())
    }

    pub async fn set_playback_volume(&self, volume: f32) -> Result<(), anyhow::Error> {
        let audio = self.audio_state.lock().await;
        if let Some(state) = audio.as_ref() {
            let mut playback = state.playback_stream.lock().await;
            playback.set_volume(volume).await;
            tracing::debug!("Playback volume set to {:.0}%", volume * 100.0);
            Ok(())
        } else {
            Err(anyhow!("No active audio state"))
        }
    }

    pub async fn set_capture_volume(&self, volume: f32) -> Result<(), anyhow::Error> {
        let audio = self.audio_state.lock().await;
        if let Some(state) = audio.as_ref() {
            let mut capture = state.capture_stream.lock().await;
            capture.set_volume(volume).await;
            tracing::debug!("Capture volume set to {:.0}%", volume * 100.0);
            Ok(())
        } else {
            Err(anyhow!("No active audio state"))
        }
    }

    pub async fn get_capture_volume(&self) -> Result<f32, anyhow::Error> {
        let audio = self.audio_state.lock().await;
        if let Some(state) = audio.as_ref() {
            let capture = state.capture_stream.lock().await;
            Ok(capture.volume())
        } else {
            Err(anyhow!("No active audio state"))
        }
    }

    pub async fn get_playback_volume(&self) -> Result<f32, anyhow::Error> {
        let audio = self.audio_state.lock().await;
        if let Some(state) = audio.as_ref() {
            let playback = state.playback_stream.lock().await;
            Ok(playback.volume())
        } else {
            Err(anyhow!("No active audio state"))
        }
    }

    async fn start_audio_for_call(&self) -> Result<(), anyhow::Error> {
        tracing::info!("=== Starting audio for call ===");

        let current = self.current_call.read().await;
        let call_handle = current.as_ref().ok_or_else(|| anyhow!("No current call"))?;

        let call_id = call_handle.call_id();
        let peer_address = call_handle.peer_address();
        let contact_handle = call_handle.contact_handle().clone();
        let call_handle_clone = call_handle.clone();
        drop(current);

        tracing::info!(
            "Starting audio for call {} with peer {}",
            call_id,
            peer_address
        );

        // Use default codec (ADPCM)
        let codec_type = CodecType::ADPCM;

        // Create audio state with default devices
        self.create_audio_state(
            call_id,
            codec_type,
            None,
            None,
            contact_handle,
            call_handle_clone,
        )
        .await?;

        tracing::info!("=== Audio started successfully for call {} ===", call_id);
        Ok(())
    }

    async fn create_audio_state(
        &self,
        call_id: Uuid,
        codec_type: CodecType,
        input_device_name: Option<String>,
        output_device_name: Option<String>,
        contact_handle: ContactHandle,
        call_handle: CallHandle,
    ) -> Result<(), anyhow::Error> {
        tracing::info!("Creating audio state for call {}", call_id);

        // Get audio devices
        tracing::debug!("Getting audio input device: {:?}", input_device_name);
        let input_device = AudioManager::get_input_device(input_device_name.clone()).await?;
        tracing::debug!("Getting audio output device: {:?}", output_device_name);
        let output_device = AudioManager::get_output_device(output_device_name.clone()).await?;

        // Create capture stream
        tracing::debug!("Creating capture stream");
        let capture_stream = CaptureStream::new(input_device, 1.0).await?;
        let source_config =
            AudioConfig::new(capture_stream.sample_rate(), capture_stream.channels());
        let capture_stream = Arc::new(TokioMutex::new(capture_stream));
        tracing::info!(
            "Capture stream created: {}Hz, {} channels",
            source_config.sample_rate,
            source_config.channels
        );

        // Create playback stream
        tracing::debug!("Creating playback stream");
        let playback_stream = PlaybackStream::new(output_device, 1.0).await?;
        let target_config =
            AudioConfig::new(playback_stream.sample_rate(), playback_stream.channels());
        let playback_stream = Arc::new(TokioMutex::new(playback_stream));
        tracing::info!(
            "Playback stream created: {}Hz, {} channels",
            target_config.sample_rate,
            target_config.channels
        );

        // ===== AUDIO CHANNEL CONVERSION ARCHITECTURE =====
        //
        // The system handles all combinations of mono/stereo microphones and speakers:
        //
        // 1. LOCAL: Microphone (source) → Encoder → Codec → Network
        // 2. REMOTE: Network → Decoder → Speaker (target)
        //
        // 3. Encoder responsibilities:
        //    - Input: source_config (from LOCAL microphone device)
        //    - Determines codec_channels = source_config.channels.min(2)
        //    - Encodes audio with codec_channels
        //    - Sends AudioDataPacket with channels field set to codec_channels
        //
        // 4. Decoder responsibilities:
        //    - Input: AudioDataPacket from REMOTE peer (with channels field)
        //    - Decodes using AudioDataPacket.channels (from REMOTE source)
        //    - Converts to target_config.channels (LOCAL speaker)
        //    - Handles dynamic channel changes from remote peer
        //
        // SUPPORTED USE CASES (Remote → Local):
        // ┌─────────────────┬──────────────┬─────────────────────────────────┐
        // │ Remote Codec    │ Local Speaker│ Decoder Conversion              │
        // ├─────────────────┼──────────────┼─────────────────────────────────┤
        // │ Stereo (2ch)    │ Mono (1ch)   │ downmix stereo→mono             │
        // │ Stereo (2ch)    │ Stereo (2ch) │ None (perfect match)            │
        // │ Mono (1ch)      │ Stereo (2ch) │ upmix mono→stereo               │
        // │ Mono (1ch)      │ Mono (1ch)   │ None (perfect match)            │
        // └─────────────────┴──────────────┴─────────────────────────────────┘
        //
        // Key principle: Encoder determines codec channels from LOCAL source.
        //                Decoder receives codec channels from REMOTE peer via packet.
        //                Each side independently handles its own audio pipeline.

        tracing::info!(
            "Creating encoder with source config: {}Hz/{}ch, target config: {}Hz/{}ch",
            source_config.sample_rate,
            source_config.channels,
            target_config.sample_rate,
            target_config.channels
        );

        // Encoder: Uses LOCAL microphone config to determine encoding
        let encoder = Arc::new(Encoder::new(source_config, codec_type));

        // Decoder: Will determine codec channels from REMOTE peer's packets
        // Only needs to know LOCAL speaker config for final output conversion
        let decoder = Arc::new(Decoder::new(target_config, codec_type));

        tracing::info!(
            "Audio configured: {:?}, local_capture={}Hz/{}ch, local_playback={}Hz/{}ch",
            codec_type,
            source_config.sample_rate,
            source_config.channels,
            target_config.sample_rate,
            target_config.channels
        );

        // Start capture task: capture -> encoder
        let encoder_clone = encoder.clone();
        let capture_stream_for_task = capture_stream.clone();
        let call_handle_for_capture = call_handle.clone();
        let capture_task = tokio::spawn(async move {
            tracing::info!("Capture task started");
            let mut frame_count = 0u64;
            loop {
                let frame = {
                    let mut stream = capture_stream_for_task.lock().await;
                    stream.recv().await
                };

                if let Some(mut frame) = frame {
                    frame_count += 1;
                    if frame_count % 100 == 0 {
                        tracing::debug!(
                            "Captured {} audio frames, samples: {}",
                            frame_count,
                            frame.samples.len()
                        );
                    }

                    // If muted, send silence instead of actual audio
                    if call_handle_for_capture.is_muted() {
                        if frame_count % 100 == 0 {
                            tracing::debug!("Microphone muted, sending silence");
                        }
                        frame.samples = vec![0.0f32; frame.samples.len()];
                    }

                    if let Err(e) = encoder_clone.send_frame(frame).await {
                        tracing::error!("Failed to send frame to encoder: {}", e);
                        break;
                    }
                } else {
                    tracing::warn!("Capture stream returned None");
                    break;
                }
            }
            tracing::warn!("Capture task ended after {} frames", frame_count);
        });

        // Start encoder task: encoder -> network
        let encoder_clone = encoder.clone();
        let contact_handle_clone = contact_handle.clone();
        let encoder_task = tokio::spawn(async move {
            tracing::info!("Encoder task started");
            let mut packet_count = 0u64;
            while let Some(mut packet) = encoder_clone.recv_packet().await {
                packet_count += 1;
                if packet_count % 50 == 0 {
                    tracing::debug!(
                        "Encoded and sending audio packet #{}, size: {} bytes",
                        packet_count,
                        packet.data.len()
                    );
                }
                // Set the real call_id (encoder sets it to Uuid::nil())
                packet.call_id = call_id;
                let call_packet = CallPacket::AudioData(packet);
                if let Err(e) = contact_handle_clone.send_call_packet(call_packet).await {
                    tracing::error!("Failed to send audio packet #{}: {}", packet_count, e);
                    break;
                }
            }
            tracing::warn!("Encoder task ended after {} packets", packet_count);
        });

        // Start playback task: decoder -> playback
        let decoder_clone = decoder.clone();
        let playback_stream_for_task = playback_stream.clone();
        let playback_task = tokio::spawn(async move {
            tracing::info!("Playback task started");
            let mut frame_count = 0u64;
            while let Some(frame) = decoder_clone.recv_frame().await {
                frame_count += 1;
                if frame_count % 100 == 0 {
                    tracing::debug!(
                        "Playing audio frame #{}, samples: {}",
                        frame_count,
                        frame.samples.len()
                    );
                }
                let mut stream = playback_stream_for_task.lock().await;
                if let Err(e) = stream.send(frame).await {
                    tracing::error!("Failed to send frame to playback: {}", e);
                    break;
                }
            }
            tracing::warn!("Playback task ended after {} frames", frame_count);
        });

        let audio_state = AudioState {
            decoder,
            capture_stream,
            playback_stream,
            capture_task,
            playback_task,
            encoder_task,
            input_device_name,
            output_device_name,
            codec_type,
        };

        let mut audio = self.audio_state.lock().await;
        *audio = Some(audio_state);

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
    fn drop(&mut self) {}
}
