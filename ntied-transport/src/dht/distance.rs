use crate::crypto::{PEER_ID_SIZE, PeerId};

const HASH_SIZE: usize = PEER_ID_SIZE - 1;

pub type Distance = [u8; HASH_SIZE];

pub fn xor_distance(a: &PeerId, b: &PeerId) -> Distance {
    let a_bytes = a.to_bytes();
    let b_bytes = b.to_bytes();
    let mut result = [0u8; HASH_SIZE];
    for i in 0..HASH_SIZE {
        result[i] = a_bytes[i + 1] ^ b_bytes[i + 1];
    }
    result
}

pub fn leading_zeros(distance: &Distance) -> usize {
    let mut count = 0;
    for &byte in distance {
        if byte == 0 {
            count += 8;
        } else {
            count += byte.leading_zeros() as usize;
            break;
        }
    }
    count
}

pub fn bucket_index(local: &PeerId, remote: &PeerId) -> Option<usize> {
    let dist = xor_distance(local, remote);
    let lz = leading_zeros(&dist);
    if lz >= HASH_SIZE * 8 {
        return None;
    }
    Some(HASH_SIZE * 8 - 1 - lz)
}

pub fn is_closer(a: &PeerId, b: &PeerId, target: &PeerId) -> bool {
    let da = xor_distance(a, target);
    let db = xor_distance(b, target);
    da < db
}

pub fn sort_by_distance(nodes: &mut [PeerId], target: &PeerId) {
    nodes.sort_by(|a, b| {
        let da = xor_distance(a, target);
        let db = xor_distance(b, target);
        da.cmp(&db)
    });
}
