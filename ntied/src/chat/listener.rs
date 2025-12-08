use async_trait::async_trait;
use ntied_crypto::PublicKey;

use crate::models::Message;

#[async_trait]
pub trait ChatListener: Send + Sync {
    async fn on_incoming_message(&self, public_key: PublicKey, message: Message);

    async fn on_outgoing_message(&self, public_key: PublicKey, message: Message);
}

pub(super) struct StubListener;

#[async_trait]
impl ChatListener for StubListener {
    async fn on_incoming_message(&self, public_key: PublicKey, message: Message) {
        _ = public_key;
        _ = message;
    }

    async fn on_outgoing_message(&self, public_key: PublicKey, message: Message) {
        _ = public_key;
        _ = message;
    }
}
