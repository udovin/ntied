use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ntied_transport::Address;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::contact::ContactHandle;

use super::CallListener;

#[derive(Debug, Clone, PartialEq)]
pub enum CallState {
    Idle,
    Calling,
    Ringing,
    Connected,
    Ended,
}

#[derive(Clone)]
pub struct CallHandle {
    call_id: Uuid,
    peer_address: Address,
    is_incoming: bool,
    contact_handle: ContactHandle,
    state: Arc<RwLock<CallState>>,
    is_muted: Arc<AtomicBool>,
    listener: Arc<dyn CallListener>,
}

impl CallHandle {
    pub fn new(
        call_id: Uuid,
        peer_address: Address,
        is_incoming: bool,
        contact_handle: ContactHandle,
        listener: Arc<dyn CallListener>,
    ) -> Self {
        Self {
            call_id,
            peer_address,
            is_incoming,
            contact_handle,
            state: Arc::new(RwLock::new(CallState::Idle)),
            is_muted: Arc::new(AtomicBool::new(false)),
            listener,
        }
    }

    pub fn call_id(&self) -> Uuid {
        self.call_id
    }

    pub fn peer_address(&self) -> Address {
        self.peer_address
    }

    pub fn is_incoming(&self) -> bool {
        self.is_incoming
    }

    pub fn contact_handle(&self) -> ContactHandle {
        self.contact_handle.clone()
    }

    pub async fn get_state(&self) -> CallState {
        self.state.read().await.clone()
    }

    pub async fn set_state(&self, state: CallState) {
        let mut current_state = self.state.write().await;
        *current_state = state.clone();

        // Notify listener of state change
        let state_str = match state {
            CallState::Idle => "idle",
            CallState::Calling => "calling",
            CallState::Ringing => "ringing",
            CallState::Connected => "connected",
            CallState::Ended => "ended",
        };
        self.listener
            .on_call_state_changed(self.peer_address, state_str)
            .await;
    }

    pub async fn toggle_mute(&self) -> Result<bool, anyhow::Error> {
        let was_muted = self.is_muted.load(Ordering::Relaxed);
        self.is_muted.store(!was_muted, Ordering::Relaxed);
        Ok(!was_muted)
    }

    pub fn is_muted(&self) -> bool {
        self.is_muted.load(Ordering::Relaxed)
    }
}
