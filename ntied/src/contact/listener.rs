use async_trait::async_trait;
use ntied_transport::PeerId;

use crate::packet::ContactProfile;

#[async_trait]
pub trait ContactListener: Send + Sync {
    async fn on_server_connected(&self);

    async fn on_server_disconnected(&self);

    async fn on_contact_connected(&self, peer_id: PeerId);

    async fn on_contact_disconnected(&self, peer_id: PeerId);

    /// Path the peer connection is currently using (true = relayed, false =
    /// direct). Fires once after `on_contact_connected` and again whenever the
    /// path flips (e.g. after a successful hole-punch upgrade).
    async fn on_contact_connection_path(&self, peer_id: PeerId, is_relayed: bool);

    async fn on_contact_incoming(&self, peer_id: PeerId, profile: ContactProfile);

    async fn on_contact_accepted(&self, peer_id: PeerId, profile: ContactProfile);

    async fn on_contact_rejected(&self, peer_id: PeerId);
}

pub(super) struct StubListener;

#[async_trait]
impl ContactListener for StubListener {
    async fn on_server_connected(&self) {}

    async fn on_server_disconnected(&self) {}

    async fn on_contact_connected(&self, _peer_id: PeerId) {}

    async fn on_contact_disconnected(&self, _peer_id: PeerId) {}

    async fn on_contact_connection_path(&self, _peer_id: PeerId, _is_relayed: bool) {}

    async fn on_contact_incoming(&self, _peer_id: PeerId, _profile: ContactProfile) {}

    async fn on_contact_accepted(&self, _peer_id: PeerId, _profile: ContactProfile) {}

    async fn on_contact_rejected(&self, _peer_id: PeerId) {}
}
