use std::net::SocketAddr;

use crate::crypto::{PeerId, PEER_ID_SIZE};
use crate::wire::{Reader, Writer};

pub const PURPOSE_RELAY: u16 = 0x0001;

const MSG_WELCOME: u8 = 0x01;
const MSG_TUNNEL: u8 = 0x02;
const MSG_HOLE_PUNCH_REQUEST: u8 = 0x03;
const MSG_HOLE_PUNCH_NOTIFY: u8 = 0x04;

#[derive(Debug, Clone)]
pub enum RelayMessage {
    /// Relay → Client: welcome after channel open, contains external address
    Welcome { external_addr: SocketAddr },
    /// Bidirectional: tunnel a raw packet to/from a peer
    Tunnel { peer_id: PeerId, data: Vec<u8> },
    /// Client → Relay: request hole punch to target
    HolePunchRequest { target: PeerId },
    /// Relay → Client: notify about incoming hole punch
    HolePunchNotify {
        requester: PeerId,
        addrs: Vec<SocketAddr>,
    },
}

impl RelayMessage {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        match self {
            Self::Welcome { external_addr } => {
                w.write_u8(MSG_WELCOME);
                w.write_socket_addr(external_addr);
            }
            Self::Tunnel { peer_id, data } => {
                w.write_u8(MSG_TUNNEL);
                w.write_bytes(&peer_id.to_bytes());
                w.write_bytes(data);
            }
            Self::HolePunchRequest { target } => {
                w.write_u8(MSG_HOLE_PUNCH_REQUEST);
                w.write_bytes(&target.to_bytes());
            }
            Self::HolePunchNotify { requester, addrs } => {
                w.write_u8(MSG_HOLE_PUNCH_NOTIFY);
                w.write_bytes(&requester.to_bytes());
                w.write_u8(addrs.len() as u8);
                for addr in addrs {
                    w.write_socket_addr(addr);
                }
            }
        }
        w.into_vec()
    }

    pub fn decode(data: &[u8]) -> Option<Self> {
        let mut r = Reader::new(data);
        let msg_type = r.read_u8().ok()?;
        match msg_type {
            MSG_WELCOME => {
                let external_addr = r.read_socket_addr().ok()?;
                Some(Self::Welcome { external_addr })
            }
            MSG_TUNNEL => {
                let peer_id = PeerId::from_bytes(r.read_array::<PEER_ID_SIZE>().ok()?);
                let data = r.remaining().to_vec();
                Some(Self::Tunnel { peer_id, data })
            }
            MSG_HOLE_PUNCH_REQUEST => {
                let target = PeerId::from_bytes(r.read_array::<PEER_ID_SIZE>().ok()?);
                Some(Self::HolePunchRequest { target })
            }
            MSG_HOLE_PUNCH_NOTIFY => {
                let requester = PeerId::from_bytes(r.read_array::<PEER_ID_SIZE>().ok()?);
                let count = r.read_u8().ok()? as usize;
                let mut addrs = Vec::with_capacity(count);
                for _ in 0..count {
                    addrs.push(r.read_socket_addr().ok()?);
                }
                Some(Self::HolePunchNotify { requester, addrs })
            }
            _ => None,
        }
    }
}
