use async_trait::async_trait;
use ntied_transport::Address;

use crate::models::Message;

#[async_trait]
pub trait ChatListener: Send + Sync {
    async fn on_incoming_message(&self, address: Address, message: Message);

    async fn on_outgoing_message(&self, address: Address, message: Message);
}

pub(super) struct StubListener;

#[async_trait]
impl ChatListener for StubListener {
    async fn on_incoming_message(&self, address: Address, message: Message) {
        _ = address;
        _ = message;
    }

    async fn on_outgoing_message(&self, address: Address, message: Message) {
        _ = address;
        _ = message;
    }
}
