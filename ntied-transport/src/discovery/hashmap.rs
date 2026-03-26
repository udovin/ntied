use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::RwLock;

use async_trait::async_trait;

use super::{Discovery, RouteInfo};
use crate::crypto::PeerId;

pub struct HashMapDiscovery {
    map: RwLock<HashMap<PeerId, SocketAddr>>,
}

impl HashMapDiscovery {
    pub fn new() -> Self {
        Self {
            map: RwLock::new(HashMap::new()),
        }
    }
}

#[async_trait]
impl Discovery for HashMapDiscovery {
    async fn resolve(&self, peer_id: &PeerId) -> Option<RouteInfo> {
        self.map
            .read()
            .unwrap()
            .get(peer_id)
            .copied()
            .map(RouteInfo::Direct)
    }

    async fn register(&self, peer_id: PeerId, addr: SocketAddr) {
        self.map.write().unwrap().insert(peer_id, addr);
    }
}
