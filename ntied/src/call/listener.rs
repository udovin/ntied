use async_trait::async_trait;
use ntied_transport::PeerId;

#[async_trait]
pub trait CallListener: Send + Sync {
    async fn on_incoming_call(&self, peer_id: PeerId);
    async fn on_outgoing_call(&self, peer_id: PeerId);
    async fn on_call_accepted(&self, peer_id: PeerId);
    async fn on_call_rejected(&self, peer_id: PeerId);
    /// Called when call is connected. is_muted indicates initial microphone state (always false for new calls)
    async fn on_call_connected(&self, peer_id: PeerId);
    async fn on_call_ended(&self, peer_id: PeerId, reason: &str);
    async fn on_call_state_changed(&self, peer_id: PeerId, state: &str);
    async fn on_audio_data_received(&self, peer_id: PeerId, data: Vec<u8>);
    async fn on_video_frame_received(&self, peer_id: PeerId, frame: Vec<u8>);
}

pub struct StubListener;

#[async_trait]
impl CallListener for StubListener {
    async fn on_incoming_call(&self, _peer_id: PeerId) {}
    async fn on_outgoing_call(&self, _peer_id: PeerId) {}
    async fn on_call_accepted(&self, _peer_id: PeerId) {}
    async fn on_call_rejected(&self, _peer_id: PeerId) {}
    async fn on_call_connected(&self, _peer_id: PeerId) {}
    async fn on_call_ended(&self, _peer_id: PeerId, _reason: &str) {}
    async fn on_call_state_changed(&self, _peer_id: PeerId, _state: &str) {}
    async fn on_audio_data_received(&self, _peer_id: PeerId, _data: Vec<u8>) {}
    async fn on_video_frame_received(&self, _peer_id: PeerId, _frame: Vec<u8>) {}
}
