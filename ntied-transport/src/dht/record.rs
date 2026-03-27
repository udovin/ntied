use std::net::SocketAddr;

use crate::crypto::{
    PEER_ID_SIZE, PUBLIC_KEY_SIZE, PeerId, PrivateKey, PublicKey, SIGNATURE_SIZE, Signature,
};
use crate::wire::{CodecError, Reader, Writer};

pub const ROUTING_POLICY_OPEN: u8 = 0x00;
pub const ROUTING_POLICY_GATEWAY_RESTRICTED: u8 = 0x01;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutingPolicy {
    Open,
    GatewayRestricted(Vec<PeerId>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayInfo {
    pub gateway_peer_id: PeerId,
    pub addrs: Vec<SocketAddr>,
    pub latency_hint: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DhtNode {
    pub peer_id: PeerId,
    pub addrs: Vec<SocketAddr>,
}

#[derive(Debug, Clone)]
pub struct DhtRecord {
    pub peer_id: PeerId,
    pub public_key: PublicKey,
    pub gateways: Vec<GatewayInfo>,
    pub routing_policy: RoutingPolicy,
    pub version: u64,
    pub expires_at: u64,
    pub signature: Signature,
}

impl DhtRecord {
    pub fn sign(
        peer_id: PeerId,
        public_key: PublicKey,
        gateways: Vec<GatewayInfo>,
        routing_policy: RoutingPolicy,
        version: u64,
        expires_at: u64,
        private_key: &PrivateKey,
    ) -> Self {
        let message =
            Self::signature_input(&peer_id, &gateways, &routing_policy, version, expires_at);
        let signature = private_key.sign(&message);
        Self {
            peer_id,
            public_key,
            gateways,
            routing_policy,
            version,
            expires_at,
            signature,
        }
    }

    pub fn verify(&self) -> bool {
        let expected_peer_id = self.public_key.peer_id();
        if expected_peer_id != self.peer_id {
            return false;
        }
        let message = Self::signature_input(
            &self.peer_id,
            &self.gateways,
            &self.routing_policy,
            self.version,
            self.expires_at,
        );
        self.public_key.verify(&message, &self.signature)
    }

    fn signature_input(
        peer_id: &PeerId,
        gateways: &[GatewayInfo],
        routing_policy: &RoutingPolicy,
        version: u64,
        expires_at: u64,
    ) -> Vec<u8> {
        let mut w = Writer::new();
        w.write_bytes(&peer_id.to_bytes());
        encode_gateways(&mut w, gateways);
        encode_routing_policy(&mut w, routing_policy);
        w.write_u64(version);
        w.write_u64(expires_at);
        w.into_vec()
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.write_bytes(&self.peer_id.to_bytes());
        w.write_bytes(&self.public_key.to_bytes());
        encode_gateways(&mut w, &self.gateways);
        encode_routing_policy(&mut w, &self.routing_policy);
        w.write_u64(self.version);
        w.write_u64(self.expires_at);
        w.write_bytes(&self.signature.to_bytes());
        w.into_vec()
    }

    pub fn decode(data: &[u8]) -> Result<Self, CodecError> {
        let mut r = Reader::new(data);
        let peer_id = PeerId::from_bytes(r.read_array::<PEER_ID_SIZE>()?);
        let pk_bytes: [u8; PUBLIC_KEY_SIZE] = r.read_array()?;
        let public_key = PublicKey::from_bytes(&pk_bytes).ok_or(CodecError::UnexpectedEnd)?;
        let gateways = decode_gateways(&mut r)?;
        let routing_policy = decode_routing_policy(&mut r)?;
        let version = r.read_u64()?;
        let expires_at = r.read_u64()?;
        let sig_bytes: [u8; SIGNATURE_SIZE] = r.read_array()?;
        let signature = Signature::from_bytes(&sig_bytes).ok_or(CodecError::UnexpectedEnd)?;
        Ok(Self {
            peer_id,
            public_key,
            gateways,
            routing_policy,
            version,
            expires_at,
            signature,
        })
    }
}

impl DhtNode {
    pub fn encode(&self, w: &mut Writer) {
        w.write_bytes(&self.peer_id.to_bytes());
        w.write_u8(self.addrs.len() as u8);
        for addr in &self.addrs {
            w.write_socket_addr(addr);
        }
    }

    pub fn decode(r: &mut Reader) -> Result<Self, CodecError> {
        let peer_id = PeerId::from_bytes(r.read_array::<PEER_ID_SIZE>()?);
        let addr_count = r.read_u8()? as usize;
        let mut addrs = Vec::with_capacity(addr_count);
        for _ in 0..addr_count {
            addrs.push(r.read_socket_addr()?);
        }
        Ok(Self { peer_id, addrs })
    }
}

impl GatewayInfo {
    fn encode(&self, w: &mut Writer) {
        w.write_bytes(&self.gateway_peer_id.to_bytes());
        w.write_u8(self.addrs.len() as u8);
        for addr in &self.addrs {
            w.write_socket_addr(addr);
        }
        w.write_u16(self.latency_hint);
    }

    fn decode(r: &mut Reader) -> Result<Self, CodecError> {
        let gateway_peer_id = PeerId::from_bytes(r.read_array::<PEER_ID_SIZE>()?);
        let addr_count = r.read_u8()? as usize;
        let mut addrs = Vec::with_capacity(addr_count);
        for _ in 0..addr_count {
            addrs.push(r.read_socket_addr()?);
        }
        let latency_hint = r.read_u16()?;
        Ok(Self {
            gateway_peer_id,
            addrs,
            latency_hint,
        })
    }
}

fn encode_gateways(w: &mut Writer, gateways: &[GatewayInfo]) {
    w.write_u8(gateways.len() as u8);
    for gw in gateways {
        gw.encode(w);
    }
}

fn decode_gateways(r: &mut Reader) -> Result<Vec<GatewayInfo>, CodecError> {
    let count = r.read_u8()? as usize;
    let mut gateways = Vec::with_capacity(count);
    for _ in 0..count {
        gateways.push(GatewayInfo::decode(r)?);
    }
    Ok(gateways)
}

fn encode_routing_policy(w: &mut Writer, policy: &RoutingPolicy) {
    match policy {
        RoutingPolicy::Open => {
            w.write_u8(ROUTING_POLICY_OPEN);
        }
        RoutingPolicy::GatewayRestricted(gws) => {
            w.write_u8(ROUTING_POLICY_GATEWAY_RESTRICTED);
            w.write_u8(gws.len() as u8);
            for gw in gws {
                w.write_bytes(&gw.to_bytes());
            }
        }
    }
}

fn decode_routing_policy(r: &mut Reader) -> Result<RoutingPolicy, CodecError> {
    let tag = r.read_u8()?;
    match tag {
        ROUTING_POLICY_OPEN => Ok(RoutingPolicy::Open),
        ROUTING_POLICY_GATEWAY_RESTRICTED => {
            let count = r.read_u8()? as usize;
            let mut gws = Vec::with_capacity(count);
            for _ in 0..count {
                gws.push(PeerId::from_bytes(r.read_array::<PEER_ID_SIZE>()?));
            }
            Ok(RoutingPolicy::GatewayRestricted(gws))
        }
        _ => Err(CodecError::UnexpectedEnd),
    }
}
