use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::anyhow;
use ntied_transport::PeerId;
use tokio::sync::{Mutex as TokioMutex, RwLock, watch};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::audio::{
    AudioConfig, AudioManager, CaptureStream, CodecManager, CodecType, Decoder, Encoder,
    PlaybackStream,
};
use crate::contact::{ContactHandle, ContactManager};
use crate::packet::{
    AudioDataPacket, CallAcceptPacket, CallEndPacket, CallPacket, CallRejectPacket,
    CallStartPacket, CodecAnswerPacket, CodecOfferPacket, VideoAnswerPacket, VideoCodec,
    VideoDataPacket, VideoOfferPacket, VideoStopPacket,
};
use crate::transport::{CallChannel, CallVideoChannel};
use crate::video::{
    MonitorSize, ScreenCaptureStream, VideoDecoder, VideoEncoder, VideoFrame,
    encoder::EncoderConfig as VideoEncoderConfig,
};

use super::{CallHandle, CallListener, CallState, StubListener};

/// Screen-share state attached to the active call. Dropped → all tasks
/// abort and the video channel goes down (audio channel is independent).
struct VideoState {
    _call_id: Uuid,
    _channel: Arc<CallVideoChannel>,
    tasks: Vec<JoinHandle<()>>,
    is_sender: bool,
}

impl Drop for VideoState {
    fn drop(&mut self) {
        tracing::info!("VideoState::drop — aborting {} task(s)", self.tasks.len());
        for t in self.tasks.drain(..) {
            t.abort();
        }
    }
}

/// Audio state for the active call - only one can exist at a time
struct AudioState {
    decoder: Arc<Decoder>,
    capture_stream: Arc<TokioMutex<CaptureStream>>,
    playback_stream: Arc<TokioMutex<PlaybackStream>>,
    capture_task: JoinHandle<()>,
    playback_task: JoinHandle<()>,
    encoder_task: JoinHandle<()>,
    datagram_recv_task: JoinHandle<()>,
    call_channel: Arc<CallChannel>,
    input_device_name: Option<String>,
    output_device_name: Option<String>,
    codec_type: CodecType,
}

impl Drop for AudioState {
    fn drop(&mut self) {
        tracing::info!("AudioState::drop — aborting capture/playback/encoder/datagram-recv tasks");
        self.capture_task.abort();
        self.playback_task.abort();
        self.encoder_task.abort();
        self.datagram_recv_task.abort();
        tracing::info!("AudioState::drop — all tasks signalled; streams will clean up as fields drop");
    }
}

pub struct CallManager {
    contact_manager: Arc<ContactManager>,
    active_calls: Arc<RwLock<HashMap<PeerId, CallHandle>>>,
    current_call: Arc<RwLock<Option<CallHandle>>>,
    listener: Arc<dyn CallListener>,
    polling_tasks: Arc<TokioMutex<HashMap<PeerId, JoinHandle<()>>>>,
    audio_state: Arc<TokioMutex<Option<AudioState>>>,
    video_state: Arc<TokioMutex<Option<VideoState>>>,
    /// UI subscribes here to observe the most recent decoded remote
    /// video frame. Published at receive-task rate, coalescing: if the
    /// UI is slow, older frames are simply overwritten.
    video_frame_tx: watch::Sender<Option<Arc<VideoFrame>>>,
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
        let (video_frame_tx, _) = watch::channel::<Option<Arc<VideoFrame>>>(None);

        let manager = Arc::new(Self {
            contact_manager,
            active_calls: Arc::new(RwLock::new(HashMap::new())),
            current_call: Arc::new(RwLock::new(None)),
            listener,
            polling_tasks: Arc::new(TokioMutex::new(HashMap::new())),
            audio_state: Arc::new(TokioMutex::new(None)),
            video_state: Arc::new(TokioMutex::new(None)),
            video_frame_tx,
            codec_manager,
        });

        // Start main polling coordinator task
        let manager_clone = manager.clone();
        tokio::spawn(manager_clone.manage_polling_tasks());

        manager
    }

    pub async fn start_call(&self, peer_id: PeerId) -> Result<CallHandle, anyhow::Error> {
        tracing::info!("Starting call to peer_id: {}", peer_id);

        // Check if already in a call
        let current = self.current_call.read().await;
        if current.is_some() {
            tracing::warn!("Cannot start call - already in a call");
            return Err(anyhow!("Already in a call"));
        }
        drop(current);

        // Get contact handle
        let contact_handle = self.contact_manager.connect_contact(peer_id).await;
        if !contact_handle.is_connected() {
            tracing::error!("Cannot start call - contact {} is not connected", peer_id);
            return Err(anyhow!("Contact is not connected"));
        }
        tracing::debug!("Contact {} is connected, proceeding with call", peer_id);

        // Create call handle
        let call_id = Uuid::now_v7();
        let call_handle = CallHandle::new(
            call_id,
            peer_id,
            false, // outgoing
            contact_handle.clone(),
            self.listener.clone(),
        );

        // Store call handle
        let mut calls = self.active_calls.write().await;
        calls.insert(peer_id, call_handle.clone());
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
            peer_id,
            call_id
        );

        self.listener.on_outgoing_call(peer_id).await;

        Ok(call_handle)
    }

    async fn handle_incoming_call(
        &self,
        peer_id: PeerId,
        packet: CallStartPacket,
    ) -> Result<(), anyhow::Error> {
        tracing::info!(
            "Received incoming call from {}, call_id: {}",
            peer_id,
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
                    peer_id
                );
                drop(current);
                self.reject_incoming_call(peer_id, packet.call_id).await?;
                return Ok(());
            }
        }
        drop(current);

        // Get or create contact handle
        let contact_handle = self.contact_manager.connect_contact(peer_id).await;
        if !contact_handle.is_connected() {
            tracing::error!(
                "Cannot accept incoming call - contact {} is not connected",
                peer_id
            );
            return Err(anyhow!("Contact is not connected"));
        }

        // Create call handle
        let call_handle = CallHandle::new(
            packet.call_id,
            peer_id,
            true, // incoming
            contact_handle.clone(),
            self.listener.clone(),
        );

        // Store as active call
        let mut calls = self.active_calls.write().await;
        calls.insert(peer_id, call_handle.clone());
        drop(calls);

        let mut current = self.current_call.write().await;
        *current = Some(call_handle.clone());
        drop(current);

        call_handle.set_state(CallState::Ringing).await;

        self.listener.on_incoming_call(peer_id).await;

        Ok(())
    }

    pub async fn accept_call(&self, peer_id: PeerId) -> Result<(), anyhow::Error> {
        tracing::info!("Accepting call from {}", peer_id);

        let current = self.current_call.read().await;
        let call_handle = current.as_ref().ok_or_else(|| anyhow!("No current call"))?;

        if call_handle.peer_id() != peer_id {
            return Err(anyhow!("Current call is not from {}", peer_id));
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

        // The "call connected" notification is mixed into the call's own
        // PlaybackStream inside start_audio_for_call — playing it through
        // a separate cpal stream here races for the device and goes silent.
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
        self.listener.on_call_accepted(peer_id).await;

        self.listener.on_call_connected(peer_id).await;

        tracing::info!("Call accepted from {}", peer_id);
        Ok(())
    }

    pub async fn reject_call(&self, peer_id: PeerId) -> Result<(), anyhow::Error> {
        tracing::info!("Rejecting call from {}", peer_id);

        let current = self.current_call.read().await;
        let call_handle = current.as_ref().ok_or_else(|| anyhow!("No current call"))?;

        if call_handle.peer_id() != peer_id {
            return Err(anyhow!("Current call is not from {}", peer_id));
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
        self.cleanup_call(peer_id).await;

        // Notify listener
        self.listener.on_call_rejected(peer_id).await;
        self.listener.on_call_ended(peer_id, "Call rejected").await;

        tracing::info!("Call rejected from {}", peer_id);
        Ok(())
    }

    async fn reject_incoming_call(
        &self,
        peer_id: PeerId,
        call_id: Uuid,
    ) -> Result<(), anyhow::Error> {
        tracing::debug!("Rejecting incoming call from {} (busy)", peer_id);

        let contact_handle = self.contact_manager.connect_contact(peer_id).await;

        let packet = CallPacket::Reject(CallRejectPacket { call_id });
        contact_handle
            .send_call_packet(packet)
            .await
            .map_err(|e| anyhow!("Failed to send reject packet: {}", e))?;

        Ok(())
    }

    pub async fn end_call(&self, peer_id: PeerId) -> Result<(), anyhow::Error> {
        tracing::info!("Ending call with {}", peer_id);

        let current = self.current_call.read().await;
        let call_handle = current.as_ref().ok_or_else(|| anyhow!("No current call"))?;

        if call_handle.peer_id() != peer_id {
            return Err(anyhow!("Current call is not with {}", peer_id));
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
        self.cleanup_call(peer_id).await;

        // Notify listener
        self.listener.on_call_ended(peer_id, "Call ended").await;

        tracing::info!("Call ended with {}", peer_id);
        Ok(())
    }

    async fn handle_call_accepted(
        &self,
        peer_id: PeerId,
        _packet: CallAcceptPacket,
    ) -> Result<(), anyhow::Error> {
        tracing::info!("Call accepted by {}", peer_id);

        let current = self.current_call.read().await;
        if let Some(call_handle) = current.as_ref() {
            if call_handle.peer_id() == peer_id {
                call_handle.set_state(CallState::Connected).await;
                drop(current);

                // Notification is mixed into the call's PlaybackStream by
                // start_audio_for_call (see comment in accept_call).
                if let Err(e) = self.start_audio_for_call().await {
                    tracing::error!("Failed to start audio for call: {}", e);
                }

                self.listener.on_call_connected(peer_id).await;
            }
        }

        Ok(())
    }

    async fn handle_call_rejected(
        &self,
        peer_id: PeerId,
        _packet: CallRejectPacket,
    ) -> Result<(), anyhow::Error> {
        tracing::info!("Call rejected by {}", peer_id);

        self.cleanup_call(peer_id).await;
        self.listener.on_call_rejected(peer_id).await;
        self.listener.on_call_ended(peer_id, "Call rejected").await;

        Ok(())
    }

    async fn handle_call_ended(
        &self,
        peer_id: PeerId,
        _packet: CallEndPacket,
    ) -> Result<(), anyhow::Error> {
        tracing::info!("Call ended by {}", peer_id);

        self.cleanup_call(peer_id).await;
        self.listener
            .on_call_ended(peer_id, "Remote ended call")
            .await;

        Ok(())
    }

    async fn handle_codec_offer(
        &self,
        peer_id: PeerId,
        packet: CodecOfferPacket,
    ) -> Result<(), anyhow::Error> {
        tracing::debug!(
            "Received codec offer from {}: preferred={:?}",
            peer_id,
            packet.preferred_codec.codec
        );

        // Check if this is for our current call
        let current = self.current_call.read().await;
        let call_handle = current.as_ref().ok_or_else(|| anyhow!("No current call"))?;

        if call_handle.peer_id() != peer_id {
            tracing::warn!(
                "Received codec offer from {} but current call is with {}",
                peer_id,
                call_handle.peer_id()
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
        peer_id: PeerId,
        packet: CodecAnswerPacket,
    ) -> Result<(), anyhow::Error> {
        tracing::info!(
            "Received codec answer from {}, negotiated: {:?}",
            peer_id,
            packet.negotiated_codec.codec
        );

        // Just log it - codec is already set when creating AudioState
        Ok(())
    }

    async fn cleanup_call(&self, peer_id: PeerId) {
        // Set call state to Ended before cleanup
        let current = self.current_call.read().await;
        if let Some(call) = current.as_ref() {
            if call.peer_id() == peer_id {
                call.set_state(CallState::Ended).await;
            }
        }
        let is_current_call = current
            .as_ref()
            .map(|c| c.peer_id() == peer_id)
            .unwrap_or(false);
        drop(current);

        if is_current_call {
            let mut audio = self.audio_state.lock().await;
            if audio.take().is_some() {
                tracing::debug!("Audio state stopped for peer_id {}", peer_id);
            }
            let mut video = self.video_state.lock().await;
            if video.take().is_some() {
                tracing::debug!("Video state stopped for peer_id {}", peer_id);
            }
            let _ = self.video_frame_tx.send(None);
        }

        let mut calls = self.active_calls.write().await;
        calls.remove(&peer_id);
        drop(calls);

        let mut current = self.current_call.write().await;
        if let Some(call) = current.as_ref() {
            if call.peer_id() == peer_id {
                *current = None;
            }
        }
    }

    pub async fn get_current_call(&self) -> Option<CallHandle> {
        self.current_call.read().await.clone()
    }

    /// Subscribe to decoded remote video frames. UI observes the latest
    /// frame; coalesces if the UI thread is slower than the decode rate.
    pub fn subscribe_video_frames(&self) -> watch::Receiver<Option<Arc<VideoFrame>>> {
        self.video_frame_tx.subscribe()
    }

    /// Start sharing the primary monitor to the active call peer. Sends
    /// a `VideoOffer`; the actual channel opening happens once the peer
    /// answers via `handle_video_answer`.
    pub async fn start_screen_share(&self) -> Result<(), anyhow::Error> {
        let current = self.current_call.read().await;
        let call = current
            .as_ref()
            .ok_or_else(|| anyhow!("no active call"))?
            .clone();
        drop(current);

        if self.video_state.lock().await.is_some() {
            return Err(anyhow!("screen share already in progress"));
        }

        let size = MonitorSize::primary().map_err(|e| anyhow!("primary monitor: {e}"))?;
        tracing::info!(
            "Starting screen share at {}x{}@30fps (pending peer answer)",
            size.width,
            size.height
        );

        let contact_handle = self
            .contact_manager
            .connect_contact(call.peer_id())
            .await;

        let offer = VideoOfferPacket {
            call_id: call.call_id(),
            width: size.width,
            height: size.height,
            framerate: 30,
            codec: VideoCodec::H264,
        };
        contact_handle
            .send_call_packet(CallPacket::VideoOffer(offer))
            .await
            .map_err(|e| anyhow!("send VideoOffer: {e}"))?;

        Ok(())
    }

    /// Stop sending video to the peer. No-op if we weren't sharing.
    pub async fn stop_screen_share(&self) -> Result<(), anyhow::Error> {
        let current = self.current_call.read().await;
        let Some(call) = current.as_ref().cloned() else {
            return Ok(());
        };
        drop(current);

        let was_sender = {
            let mut video = self.video_state.lock().await;
            match video.take() {
                Some(state) => state.is_sender,
                None => return Ok(()),
            }
        };
        let _ = self.video_frame_tx.send(None);

        if was_sender {
            let contact_handle = self
                .contact_manager
                .connect_contact(call.peer_id())
                .await;
            let _ = contact_handle
                .send_call_packet(CallPacket::VideoStop(VideoStopPacket {
                    call_id: call.call_id(),
                }))
                .await;
        }
        Ok(())
    }

    async fn handle_video_offer(
        &self,
        peer_id: PeerId,
        packet: VideoOfferPacket,
    ) -> Result<(), anyhow::Error> {
        let current = self.current_call.read().await;
        let Some(call) = current.as_ref().cloned() else {
            return Ok(());
        };
        if call.peer_id() != peer_id || call.call_id() != packet.call_id {
            return Ok(());
        }
        drop(current);

        if self.video_state.lock().await.is_some() {
            tracing::debug!("Ignoring VideoOffer — already have a video session");
            return Ok(());
        }

        let contact_handle = self.contact_manager.connect_contact(peer_id).await;
        tracing::info!(
            "Auto-accepting VideoOffer {}x{}@{}fps from {}",
            packet.width,
            packet.height,
            packet.framerate,
            peer_id
        );
        contact_handle
            .send_call_packet(CallPacket::VideoAnswer(VideoAnswerPacket {
                call_id: packet.call_id,
                accepted: true,
            }))
            .await
            .map_err(|e| anyhow!("send VideoAnswer: {e}"))?;

        let channel = Arc::new(
            contact_handle
                .accept_call_video_channel()
                .await
                .map_err(|e| anyhow!("accept video channel: {e}"))?,
        );

        let recv_task = self.spawn_video_recv_task(channel.clone());
        *self.video_state.lock().await = Some(VideoState {
            _call_id: packet.call_id,
            _channel: channel,
            tasks: vec![recv_task],
            is_sender: false,
        });
        Ok(())
    }

    async fn handle_video_answer(
        &self,
        peer_id: PeerId,
        packet: VideoAnswerPacket,
    ) -> Result<(), anyhow::Error> {
        let current = self.current_call.read().await;
        let Some(call) = current.as_ref().cloned() else {
            return Ok(());
        };
        if call.peer_id() != peer_id || call.call_id() != packet.call_id {
            return Ok(());
        }
        drop(current);

        if !packet.accepted {
            tracing::info!("Peer {} declined VideoOffer", peer_id);
            return Ok(());
        }

        if self.video_state.lock().await.is_some() {
            tracing::debug!("Ignoring VideoAnswer — video session already set up");
            return Ok(());
        }

        let contact_handle = self.contact_manager.connect_contact(peer_id).await;
        let channel = Arc::new(
            contact_handle
                .open_call_video_channel()
                .await
                .map_err(|e| anyhow!("open video channel: {e}"))?,
        );

        // Defaults give an upper bound of 1920x1080 — the encoder will
        // downscale anything bigger by an integer ratio. We deliberately
        // do NOT plug capture-monitor dimensions in here: those are the
        // *source* size, not the *cap*, and substituting them disables
        // the downscale (turning a 5120x1440 capture into a 5120x1440
        // encode, which busts openh264's 3840x2160 hard limit).
        let encoder_config = VideoEncoderConfig {
            framerate: 30,
            ..Default::default()
        };

        let send_task = self.spawn_video_send_task(channel.clone(), call.call_id(), encoder_config)?;
        *self.video_state.lock().await = Some(VideoState {
            _call_id: call.call_id(),
            _channel: channel,
            tasks: vec![send_task],
            is_sender: true,
        });
        Ok(())
    }

    async fn handle_video_stop(
        &self,
        peer_id: PeerId,
        packet: VideoStopPacket,
    ) -> Result<(), anyhow::Error> {
        let current = self.current_call.read().await;
        let Some(call) = current.as_ref().cloned() else {
            return Ok(());
        };
        if call.peer_id() != peer_id || call.call_id() != packet.call_id {
            return Ok(());
        }
        drop(current);

        if self.video_state.lock().await.take().is_some() {
            let _ = self.video_frame_tx.send(None);
            tracing::info!("Video session stopped by {}", peer_id);
        }
        Ok(())
    }

    fn spawn_video_send_task(
        &self,
        channel: Arc<CallVideoChannel>,
        call_id: Uuid,
        config: VideoEncoderConfig,
    ) -> Result<JoinHandle<()>, anyhow::Error> {
        let framerate = config.framerate;
        let mut capture = ScreenCaptureStream::start_primary_monitor(framerate)
            .map_err(|e| anyhow!("start capture: {e}"))?;
        let mut encoder = VideoEncoder::new(config).map_err(|e| anyhow!("video encoder: {e}"))?;
        encoder.request_keyframe();

        Ok(tokio::spawn(async move {
            tracing::info!("Video send task started (primary monitor)");
            let mut frame_count = 0u64;
            while let Some(frame) = capture.recv().await {
                frame_count += 1;
                let encoded = match encoder.encode(&frame) {
                    Ok(Some(bytes)) => bytes,
                    Ok(None) => continue,
                    Err(e) => {
                        tracing::warn!("encode error on frame {}: {}", frame_count, e);
                        continue;
                    }
                };
                if frame_count <= 3 || frame_count % 30 == 0 {
                    tracing::debug!(
                        "Video frame #{}: captured {}x{} stride {}, encoded {} bytes",
                        frame_count,
                        frame.width,
                        frame.height,
                        frame.stride,
                        encoded.len()
                    );
                }
                let packet = VideoDataPacket {
                    call_id,
                    timestamp: frame.captured_at.elapsed().as_micros() as u64,
                    frame: encoded,
                };
                let bytes = match bincode::serialize(&packet) {
                    Ok(b) => b,
                    Err(e) => {
                        tracing::warn!("serialize VideoDataPacket: {}", e);
                        continue;
                    }
                };
                if let Err(e) = channel.send(&bytes).await {
                    tracing::error!("video channel send: {}", e);
                    break;
                }
            }
            tracing::warn!("Video send task ended after {} frames", frame_count);
        }))
    }

    fn spawn_video_recv_task(&self, channel: Arc<CallVideoChannel>) -> JoinHandle<()> {
        let frame_tx = self.video_frame_tx.clone();
        tokio::spawn(async move {
            tracing::info!("Video recv task started");
            let mut decoder = match VideoDecoder::new() {
                Ok(d) => d,
                Err(e) => {
                    tracing::error!("video decoder init: {}", e);
                    return;
                }
            };
            let mut packet_count = 0u64;
            loop {
                let data = match channel.recv().await {
                    Ok(d) => d,
                    Err(e) => {
                        tracing::error!("video channel recv: {}", e);
                        break;
                    }
                };
                let packet: VideoDataPacket = match bincode::deserialize(&data) {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::warn!("deserialize VideoDataPacket: {}", e);
                        continue;
                    }
                };
                packet_count += 1;
                if packet_count <= 3 || packet_count % 30 == 0 {
                    tracing::debug!(
                        "Video recv #{}: {} encoded bytes",
                        packet_count,
                        packet.frame.len()
                    );
                }
                match decoder.decode(&packet.frame) {
                    Ok(Some(frame)) => {
                        if packet_count <= 3 || packet_count % 30 == 0 {
                            tracing::debug!(
                                "Decoded frame #{}: {}x{}",
                                packet_count,
                                frame.width,
                                frame.height
                            );
                        }
                        let _ = frame_tx.send(Some(Arc::new(frame)));
                    }
                    Ok(None) => {
                        tracing::trace!("Decoder needs more data (packet #{})", packet_count);
                    }
                    Err(e) => {
                        tracing::warn!("decode packet #{}: {}", packet_count, e);
                    }
                }
            }
            tracing::warn!("Video recv task ended after {} packets", packet_count);
        })
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
        let call_channel = old_state.call_channel.clone();

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
            call_channel,
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
        let call_channel = old_state.call_channel.clone();

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
            call_channel,
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
        let peer_id = call_handle.peer_id();
        let is_incoming = call_handle.is_incoming();
        let contact_handle = call_handle.contact_handle().clone();
        let call_handle_clone = call_handle.clone();
        drop(current);

        tracing::info!("Starting audio for call {} with peer {}", call_id, peer_id);

        // Open or accept the call channel (datagram) for audio data
        let call_channel = if is_incoming {
            // Responder: accept the call channel opened by the initiator
            tracing::debug!("Accepting call channel (responder)");
            contact_handle.accept_call_channel().await.map_err(|e| {
                anyhow!("Failed to accept call channel: {}", e)
            })?
        } else {
            // Initiator: open a new call channel
            tracing::debug!("Opening call channel (initiator)");
            contact_handle.open_call_channel().await.map_err(|e| {
                anyhow!("Failed to open call channel: {}", e)
            })?
        };
        tracing::info!("Call channel established for call {}", call_id);

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
            Arc::new(call_channel),
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
        _contact_handle: ContactHandle,
        call_handle: CallHandle,
        call_channel: Arc<CallChannel>,
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

        // Start encoder task: encoder -> network (via unreliable datagram CallChannel)
        let encoder_clone = encoder.clone();
        let call_channel_for_encoder = call_channel.clone();
        let encoder_task = tokio::spawn(async move {
            tracing::info!("Encoder task started (using datagram call channel)");
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
                let bytes = match bincode::serialize(&packet) {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        tracing::error!("Failed to serialize audio packet #{}: {}", packet_count, e);
                        continue;
                    }
                };
                if let Err(e) = call_channel_for_encoder.send(&bytes).await {
                    tracing::error!("Failed to send audio packet #{} via call channel: {}", packet_count, e);
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

        // Start datagram receiver task: call channel -> decoder.
        // When the channel errors (peer dropped, connection timed out), the
        // task spawns cleanup so the call doesn't linger forever.
        let call_channel_for_recv = call_channel.clone();
        let decoder_for_recv = decoder.clone();
        let cleanup_peer = call_handle.peer_id();
        let cleanup_current = self.current_call.clone();
        let cleanup_audio = self.audio_state.clone();
        let cleanup_active = self.active_calls.clone();
        let cleanup_listener = self.listener.clone();
        let datagram_recv_task = tokio::spawn(async move {
            tracing::info!("Datagram receiver task started (reading from call channel)");
            let mut packet_count = 0u64;
            loop {
                match call_channel_for_recv.recv().await {
                    Ok(data) => {
                        packet_count += 1;
                        match bincode::deserialize::<AudioDataPacket>(&data) {
                            Ok(packet) => {
                                if packet_count % 100 == 0 {
                                    tracing::debug!(
                                        "Received audio datagram #{}, size: {} bytes",
                                        packet_count,
                                        packet.data.len()
                                    );
                                }
                                if let Err(e) = decoder_for_recv.send_packet(packet).await {
                                    tracing::warn!("Failed to send packet to decoder: {}", e);
                                }
                            }
                            Err(e) => {
                                tracing::warn!("Failed to deserialize audio datagram: {}", e);
                            }
                        }
                    }
                    Err(e) => {
                        tracing::error!("Call channel recv error: {}", e);
                        break;
                    }
                }
            }
            tracing::warn!("Datagram receiver task ended after {} packets", packet_count);
            // Connection died — tear the call down. Idempotent with explicit
            // end_call: if cleanup already ran, the helper is a no-op.
            end_call_on_connection_lost(
                cleanup_peer,
                cleanup_current,
                cleanup_audio,
                cleanup_active,
                cleanup_listener,
            )
            .await;
        });

        let audio_state = AudioState {
            decoder,
            capture_stream,
            playback_stream,
            capture_task,
            playback_task,
            encoder_task,
            datagram_recv_task,
            call_channel,
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
            for peer_id in tasks.keys() {
                if !contacts
                    .iter()
                    .any(|c| c.peer_id() == Some(*peer_id) && c.is_connected())
                {
                    to_remove.push(*peer_id);
                }
            }

            for peer_id in to_remove {
                if let Some(task) = tasks.remove(&peer_id) {
                    tracing::debug!("Stopping call packet polling for {}", peer_id);
                    task.abort();
                }
            }

            // Start tasks for new connected contacts
            for contact_handle in contacts {
                if !contact_handle.is_connected() {
                    continue;
                }

                if let Some(peer_id) = contact_handle.peer_id() {
                    if !tasks.contains_key(&peer_id) {
                        // Start a dedicated polling task for this contact
                        let manager = self.clone();
                        let contact = contact_handle.clone();
                        let task = tokio::spawn(async move {
                            manager.poll_contact_packets(peer_id, contact).await;
                        });
                        tasks.insert(peer_id, task);
                        tracing::debug!("Started call packet polling for {}", peer_id);
                    }
                }
            }
        }
    }

    async fn poll_contact_packets(self: Arc<Self>, peer_id: PeerId, contact_handle: ContactHandle) {
        // Poll this specific contact continuously for call packets
        loop {
            // Check if still connected
            if !contact_handle.is_connected() {
                tracing::debug!("Contact {} disconnected, stopping polling", peer_id);
                break;
            }

            // Try to receive call packets with very short timeout
            let recv_future = contact_handle.recv_call_packet();
            let timeout_result =
                tokio::time::timeout(Duration::from_millis(100), recv_future).await;

            match timeout_result {
                Ok(Ok(packet)) => {
                    tracing::debug!("Received call packet from {}: {:?}", peer_id, packet);

                    // Process the received packet
                    if let Err(e) = self.process_call_packet(peer_id, packet).await {
                        tracing::error!("Failed to process call packet from {}: {}", peer_id, e);
                    }
                }
                Ok(Err(e)) => {
                    tracing::error!("Error receiving call packet from {}: {}", peer_id, e);
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
        peer_id: PeerId,
        packet: CallPacket,
    ) -> Result<(), anyhow::Error> {
        match packet {
            CallPacket::Start(p) => self.handle_incoming_call(peer_id, p).await,
            CallPacket::Accept(p) => self.handle_call_accepted(peer_id, p).await,
            CallPacket::Reject(p) => self.handle_call_rejected(peer_id, p).await,
            CallPacket::End(p) => self.handle_call_ended(peer_id, p).await,
            CallPacket::AudioData(_) => {
                tracing::warn!("Received AudioData on chat stream from {} - audio data should arrive via call channel datagram", peer_id);
                Ok(())
            }
            CallPacket::VideoData(_) => {
                tracing::warn!("Received VideoData on chat stream from {} - video data should arrive via call channel datagram", peer_id);
                Ok(())
            }
            CallPacket::CodecOffer(p) => self.handle_codec_offer(peer_id, p).await,
            CallPacket::CodecAnswer(p) => self.handle_codec_answer(peer_id, p).await,
            CallPacket::VideoOffer(p) => self.handle_video_offer(peer_id, p).await,
            CallPacket::VideoAnswer(p) => self.handle_video_answer(peer_id, p).await,
            CallPacket::VideoStop(p) => self.handle_video_stop(peer_id, p).await,
        }
    }
}

impl Drop for CallManager {
    fn drop(&mut self) {}
}

/// Tear down a call after the underlying transport connection died (peer
/// dropped, timeout, etc.). Mirrors `CallManager::cleanup_call` but is a
/// free function so it can be invoked from the audio receiver task without
/// holding `Arc<CallManager>`. Idempotent: a no-op when the current call
/// is already gone or belongs to a different peer.
async fn end_call_on_connection_lost(
    peer_id: PeerId,
    current_call: Arc<RwLock<Option<CallHandle>>>,
    audio_state: Arc<TokioMutex<Option<AudioState>>>,
    active_calls: Arc<RwLock<HashMap<PeerId, CallHandle>>>,
    listener: Arc<dyn CallListener>,
) {
    let still_active = {
        let current = current_call.read().await;
        current
            .as_ref()
            .map(|c| c.peer_id() == peer_id)
            .unwrap_or(false)
    };
    if !still_active {
        return;
    }
    tracing::warn!(
        ?peer_id,
        "transport connection lost during call — ending call"
    );

    {
        let current = current_call.read().await;
        if let Some(call) = current.as_ref() {
            if call.peer_id() == peer_id {
                call.set_state(CallState::Ended).await;
            }
        }
    }
    {
        let mut audio = audio_state.lock().await;
        let _ = audio.take();
    }
    {
        let mut calls = active_calls.write().await;
        calls.remove(&peer_id);
    }
    {
        let mut current = current_call.write().await;
        if let Some(call) = current.as_ref() {
            if call.peer_id() == peer_id {
                *current = None;
            }
        }
    }
    listener
        .on_call_ended(peer_id, "Connection lost")
        .await;
}
