use std::net::SocketAddr;
use std::sync::Arc;

use tokio::io;
use tokio::net::UdpSocket;

use crate::crypto::{PEER_ID_SIZE, PeerId};

use super::relay::{RelayConnection, TunnelGuard};
use crate::relay::TUNNEL_HEADER_SIZE;

/// Outbound packet sink for a single Connection.
///
/// `Udp` writes straight to a UdpSocket toward a fixed peer address.
/// `Tunnel` wraps each packet with `[peer_id]` and ships it through a
/// relay's multiplex channel; the matching `RelayConnection` pump task
/// dispatches inbound traffic back to the connection's rx.
///
/// `Tunnel::_guard` is a RAII counter so the relay knows how many live
/// tunnels reference it (used by discovery-pool shed logic).
pub(crate) enum Transport {
    Udp {
        socket: Arc<UdpSocket>,
        addr: SocketAddr,
    },
    Tunnel {
        relay: Arc<RelayConnection>,
        peer_id: PeerId,
        _guard: TunnelGuard,
    },
}

impl Transport {
    pub(crate) fn udp(socket: Arc<UdpSocket>, addr: SocketAddr) -> Arc<Self> {
        Arc::new(Self::Udp { socket, addr })
    }

    pub(crate) async fn send_packet(&self, packet: &[u8]) -> io::Result<()> {
        match self {
            Self::Udp { socket, addr } => {
                socket.send_to(packet, *addr).await?;
                Ok(())
            }
            Self::Tunnel { relay, peer_id, .. } => {
                let mut buf = Vec::with_capacity(TUNNEL_HEADER_SIZE + packet.len());
                buf.extend_from_slice(&peer_id.to_bytes());
                buf.extend_from_slice(packet);
                debug_assert_eq!(TUNNEL_HEADER_SIZE, PEER_ID_SIZE);
                relay.tunnel_channel.send(buf).await
            }
        }
    }

}
