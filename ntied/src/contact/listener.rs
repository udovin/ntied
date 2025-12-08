use async_trait::async_trait;
use ntied_crypto::PublicKey;

use crate::packet::ContactProfile;

#[async_trait]
pub trait ContactListener: Send + Sync {
    async fn on_server_connected(&self);

    async fn on_server_disconnected(&self);

    async fn on_contact_connected(&self, public_key: PublicKey);

    async fn on_contact_disconnected(&self, public_key: PublicKey);

    async fn on_contact_incoming(&self, public_key: PublicKey, profile: ContactProfile);

    async fn on_contact_accepted(&self, public_key: PublicKey, profile: ContactProfile);

    async fn on_contact_rejected(&self, public_key: PublicKey);
}

pub(super) struct StubListener;

#[async_trait]
impl ContactListener for StubListener {
    async fn on_server_connected(&self) {}

    async fn on_server_disconnected(&self) {}

    async fn on_contact_connected(&self, _public_key: PublicKey) {}

    async fn on_contact_disconnected(&self, _public_key: PublicKey) {}

    async fn on_contact_incoming(&self, _public_key: PublicKey, _profile: ContactProfile) {}

    async fn on_contact_accepted(&self, _public_key: PublicKey, _profile: ContactProfile) {}

    async fn on_contact_rejected(&self, _public_key: PublicKey) {}
}
