use async_trait::async_trait;
use ntied_transport::Address;

use crate::packet::ContactProfile;

#[async_trait]
pub trait ContactListener: Send + Sync {
    async fn on_server_connected(&self);

    async fn on_server_disconnected(&self);

    async fn on_contact_connected(&self, address: Address);

    async fn on_contact_disconnected(&self, addres: Address);

    async fn on_contact_incoming(&self, address: Address, profile: ContactProfile);

    async fn on_contact_accepted(&self, address: Address, profile: ContactProfile);

    async fn on_contact_rejected(&self, address: Address);
}

pub(super) struct StubListener;

#[async_trait]
impl ContactListener for StubListener {
    async fn on_server_connected(&self) {}

    async fn on_server_disconnected(&self) {}

    async fn on_contact_connected(&self, _address: Address) {}

    async fn on_contact_disconnected(&self, _address: Address) {}

    async fn on_contact_incoming(&self, _address: Address, _profile: ContactProfile) {}

    async fn on_contact_accepted(&self, _address: Address, _profile: ContactProfile) {}

    async fn on_contact_rejected(&self, _address: Address) {}
}
