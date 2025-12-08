use async_trait::async_trait;
use ntied_crypto::PublicKey;

#[async_trait]
pub trait CallListener: Send + Sync {
    async fn on_incoming_call(&self, public_key: PublicKey);
    async fn on_outgoing_call(&self, public_key: PublicKey);
    async fn on_call_accepted(&self, public_key: PublicKey);
    async fn on_call_rejected(&self, public_key: PublicKey);
    /// Called when call is connected. is_muted indicates initial microphone state (always false for new calls)
    async fn on_call_connected(&self, public_key: PublicKey);
    async fn on_call_ended(&self, public_key: PublicKey, reason: &str);
    async fn on_call_state_changed(&self, public_key: PublicKey, state: &str);
    async fn on_audio_data_received(&self, public_key: PublicKey, data: Vec<u8>);
    async fn on_video_frame_received(&self, public_key: PublicKey, frame: Vec<u8>);
}

pub struct StubListener;

#[async_trait]
impl CallListener for StubListener {
    async fn on_incoming_call(&self, _public_key: PublicKey) {}
    async fn on_outgoing_call(&self, _public_key: PublicKey) {}
    async fn on_call_accepted(&self, _public_key: PublicKey) {}
    async fn on_call_rejected(&self, _public_key: PublicKey) {}
    async fn on_call_connected(&self, _public_key: PublicKey) {}
    async fn on_call_ended(&self, _public_key: PublicKey, _reason: &str) {}
    async fn on_call_state_changed(&self, _public_key: PublicKey, _state: &str) {}
    async fn on_audio_data_received(&self, _public_key: PublicKey, _data: Vec<u8>) {}
    async fn on_video_frame_received(&self, _public_key: PublicKey, _frame: Vec<u8>) {}
}
