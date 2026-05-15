use async_trait::async_trait;
use ntied_transport::PeerId;

use crate::models::Message;

#[async_trait]
pub trait ChatListener: Send + Sync {
    async fn on_incoming_message(&self, peer_id: PeerId, message: Message);

    async fn on_outgoing_message(&self, peer_id: PeerId, message: Message);
}

pub(super) struct StubListener;

#[async_trait]
impl ChatListener for StubListener {
    async fn on_incoming_message(&self, _peer_id: PeerId, _message: Message) {}

    async fn on_outgoing_message(&self, _peer_id: PeerId, _message: Message) {}
}
