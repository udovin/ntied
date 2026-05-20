use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use crate::crypto::{PEER_ID_SIZE, PeerId};

/// Control-channel wire format:
/// - `[op: u8] [op-payload]`
///
/// Ops:
/// - `0x01` HolePunchRequest: client -> relay, payload = `[target_peer_id (33B)]`.
/// - `0x02` HolePunchNotify:  relay -> client, payload = `[from_peer_id (33B)] [SocketAddr]`.
///
/// `SocketAddr` is encoded as `[fam: u8] [ip] [port: u16 BE]`:
///   `fam = 4` -> V4 (4-byte IP), `fam = 6` -> V6 (16-byte IP).
const OP_HOLEPUNCH_REQUEST: u8 = 0x01;
const OP_HOLEPUNCH_NOTIFY: u8 = 0x02;

#[derive(Debug, Clone)]
pub enum ControlMsg {
    HolePunchRequest { target: PeerId },
    HolePunchNotify { from: PeerId, addr: SocketAddr },
}

impl ControlMsg {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(64);
        match self {
            Self::HolePunchRequest { target } => {
                out.push(OP_HOLEPUNCH_REQUEST);
                out.extend_from_slice(target.as_bytes());
            }
            Self::HolePunchNotify { from, addr } => {
                out.push(OP_HOLEPUNCH_NOTIFY);
                out.extend_from_slice(from.as_bytes());
                encode_sockaddr(addr, &mut out);
            }
        }
        out
    }

    pub fn decode(data: &[u8]) -> Option<Self> {
        let (&op, rest) = data.split_first()?;
        match op {
            OP_HOLEPUNCH_REQUEST => {
                if rest.len() < PEER_ID_SIZE {
                    return None;
                }
                let mut id = [0u8; PEER_ID_SIZE];
                id.copy_from_slice(&rest[..PEER_ID_SIZE]);
                Some(Self::HolePunchRequest {
                    target: PeerId::from_bytes(id),
                })
            }
            OP_HOLEPUNCH_NOTIFY => {
                if rest.len() < PEER_ID_SIZE + 1 {
                    return None;
                }
                let mut id = [0u8; PEER_ID_SIZE];
                id.copy_from_slice(&rest[..PEER_ID_SIZE]);
                let from = PeerId::from_bytes(id);
                let addr = decode_sockaddr(&rest[PEER_ID_SIZE..])?;
                Some(Self::HolePunchNotify { from, addr })
            }
            _ => None,
        }
    }
}

fn encode_sockaddr(addr: &SocketAddr, out: &mut Vec<u8>) {
    match addr.ip() {
        IpAddr::V4(ip) => {
            out.push(4);
            out.extend_from_slice(&ip.octets());
        }
        IpAddr::V6(ip) => {
            out.push(6);
            out.extend_from_slice(&ip.octets());
        }
    }
    out.extend_from_slice(&addr.port().to_be_bytes());
}

fn decode_sockaddr(data: &[u8]) -> Option<SocketAddr> {
    let (&fam, rest) = data.split_first()?;
    match fam {
        4 => {
            if rest.len() < 4 + 2 {
                return None;
            }
            let mut octets = [0u8; 4];
            octets.copy_from_slice(&rest[..4]);
            let port = u16::from_be_bytes([rest[4], rest[5]]);
            Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::from(octets)), port))
        }
        6 => {
            if rest.len() < 16 + 2 {
                return None;
            }
            let mut octets = [0u8; 16];
            octets.copy_from_slice(&rest[..16]);
            let port = u16::from_be_bytes([rest[16], rest[17]]);
            Some(SocketAddr::new(IpAddr::V6(Ipv6Addr::from(octets)), port))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_request() {
        let id = PeerId::from_bytes([7u8; PEER_ID_SIZE]);
        let msg = ControlMsg::HolePunchRequest { target: id };
        let bytes = msg.encode();
        match ControlMsg::decode(&bytes).unwrap() {
            ControlMsg::HolePunchRequest { target } => assert_eq!(target, id),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn roundtrip_notify_v4() {
        let id = PeerId::from_bytes([3u8; PEER_ID_SIZE]);
        let addr: SocketAddr = "127.0.0.1:5050".parse().unwrap();
        let msg = ControlMsg::HolePunchNotify { from: id, addr };
        let bytes = msg.encode();
        match ControlMsg::decode(&bytes).unwrap() {
            ControlMsg::HolePunchNotify { from, addr: a } => {
                assert_eq!(from, id);
                assert_eq!(a, addr);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn roundtrip_notify_v6() {
        let id = PeerId::from_bytes([9u8; PEER_ID_SIZE]);
        let addr: SocketAddr = "[::1]:8080".parse().unwrap();
        let msg = ControlMsg::HolePunchNotify { from: id, addr };
        let bytes = msg.encode();
        match ControlMsg::decode(&bytes).unwrap() {
            ControlMsg::HolePunchNotify { from, addr: a } => {
                assert_eq!(from, id);
                assert_eq!(a, addr);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn decode_unknown_op_returns_none() {
        let bytes = vec![0xff, 0x00, 0x00];
        assert!(ControlMsg::decode(&bytes).is_none());
    }
}
