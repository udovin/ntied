use crate::crypto::PEER_ID_SIZE;

/// Wire header for every multiplexed tunnel message:
/// `[other_end_peer_id (PEER_ID_SIZE)] [inner packet]`.
///
/// Outbound (us -> relay): `other_end_peer_id` = destination peer.
/// Inbound  (relay -> us): `other_end_peer_id` = source peer (relay rewrote it).
pub const TUNNEL_HEADER_SIZE: usize = PEER_ID_SIZE;
