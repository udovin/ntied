use async_trait::async_trait;
use ntied_transport::Address;

#[async_trait]
pub trait CallListener: Send + Sync {
    async fn on_incoming_call(&self, address: Address);
    async fn on_outgoing_call(&self, address: Address);
    async fn on_call_accepted(&self, address: Address);
    async fn on_call_rejected(&self, address: Address);
    /// Called when call is connected. is_muted indicates initial microphone state (always false for new calls)
    async fn on_call_connected(&self, address: Address);
    async fn on_call_ended(&self, address: Address, reason: &str);
    async fn on_call_state_changed(&self, address: Address, state: &str);
    async fn on_audio_data_received(&self, address: Address, data: Vec<u8>);
    async fn on_video_frame_received(&self, address: Address, frame: Vec<u8>);
}

pub struct StubListener;

#[async_trait]
impl CallListener for StubListener {
    async fn on_incoming_call(&self, _address: Address) {}
    async fn on_outgoing_call(&self, _address: Address) {}
    async fn on_call_accepted(&self, _address: Address) {}
    async fn on_call_rejected(&self, _address: Address) {}
    async fn on_call_connected(&self, _address: Address) {}
    async fn on_call_ended(&self, _address: Address, _reason: &str) {}
    async fn on_call_state_changed(&self, _address: Address, _state: &str) {}
    async fn on_audio_data_received(&self, _address: Address, _data: Vec<u8>) {}
    async fn on_video_frame_received(&self, _address: Address, _frame: Vec<u8>) {}
}
