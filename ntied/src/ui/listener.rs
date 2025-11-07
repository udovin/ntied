use async_trait::async_trait;
use ntied_transport::Address;
use tokio::sync::mpsc;

use crate::call::CallListener;
use crate::chat::ChatListener;
use crate::contact::ContactListener;
use crate::models::{Message, MessageKind};
use crate::packet::ContactProfile;

#[derive(Clone, Debug)]
pub enum UiEvent {
    TransportConnected(bool),
    IncomingRequest {
        name: String,
        address: String,
    },
    OutgoingRequest {
        address: String,
    },
    ContactAccepted {
        name: String,
        address: String,
    },
    ContactRemoved {
        address: String,
    },
    ContactConnection {
        address: String,
        connected: bool,
    },
    NewMessage {
        id: i64,
        address: String,
        incoming: bool,
        text: String,
    },
    MessageSent {
        id: i64,
        address: String,
        text: String,
    },
    MessageDelivered {
        id: i64,
        address: String,
    },
    // Call events
    IncomingCall {
        address: String,
        video_enabled: bool,
    },
    OutgoingCall {
        address: String,
        video_enabled: bool,
    },
    CallAccepted {
        address: String,
    },
    CallRejected {
        address: String,
    },
    CallConnected {
        address: String,
        is_muted: bool,
    },
    CallEnded {
        address: String,
        reason: String,
    },
    CallStateChanged {
        address: String,
        state: String,
    },
}

pub struct UiEventListener {
    tx: mpsc::Sender<UiEvent>,
}

impl UiEventListener {
    pub fn new(tx: mpsc::Sender<UiEvent>) -> Self {
        Self { tx }
    }
}

#[async_trait]
impl ContactListener for UiEventListener {
    async fn on_server_connected(&self) {
        if let Err(err) = self.tx.send(UiEvent::TransportConnected(true)).await {
            tracing::error!(?err, "Cannot send UI event: TransportConnected");
        }
    }

    async fn on_server_disconnected(&self) {
        if let Err(err) = self.tx.send(UiEvent::TransportConnected(false)).await {
            tracing::error!(?err, "Cannot send UI event: TransportConnected");
        }
    }

    async fn on_contact_connected(&self, address: Address) {
        if let Err(err) = self
            .tx
            .send(UiEvent::ContactConnection {
                address: address.to_string(),
                connected: true,
            })
            .await
        {
            tracing::error!(?err, "Cannot send UI event: ContactConnection");
        }
    }

    async fn on_contact_disconnected(&self, addres: Address) {
        if let Err(err) = self
            .tx
            .send(UiEvent::ContactConnection {
                address: addres.to_string(),
                connected: false,
            })
            .await
        {
            tracing::error!(?err, "Cannot send UI event: ContactConnection");
        }
    }

    async fn on_contact_incoming(&self, address: Address, profile: ContactProfile) {
        if let Err(err) = self
            .tx
            .send(UiEvent::IncomingRequest {
                name: profile.name,
                address: address.to_string(),
            })
            .await
        {
            tracing::error!(?err, "Cannot send UI event: IncomingRequest");
        }
    }

    async fn on_contact_accepted(&self, address: Address, profile: ContactProfile) {
        if let Err(err) = self
            .tx
            .send(UiEvent::ContactAccepted {
                name: profile.name,
                address: address.to_string(),
            })
            .await
        {
            tracing::error!(?err, "Cannot send UI event: ContactAccepted");
        }
    }

    async fn on_contact_rejected(&self, address: Address) {
        if let Err(err) = self
            .tx
            .send(UiEvent::ContactRemoved {
                address: address.to_string(),
            })
            .await
        {
            tracing::error!(?err, "Cannot send UI event: ContactRemoved");
        }
    }
}

#[async_trait]
impl CallListener for UiEventListener {
    async fn on_incoming_call(&self, address: Address, video_enabled: bool) {
        if let Err(err) = self
            .tx
            .send(UiEvent::IncomingCall {
                address: address.to_string(),
                video_enabled,
            })
            .await
        {
            tracing::error!(?err, "Cannot send UI event: IncomingCall");
        }
    }

    async fn on_outgoing_call(&self, address: Address, video_enabled: bool) {
        if let Err(err) = self
            .tx
            .send(UiEvent::OutgoingCall {
                address: address.to_string(),
                video_enabled,
            })
            .await
        {
            tracing::error!(?err, "Cannot send UI event: OutgoingCall");
        }
    }

    async fn on_call_accepted(&self, address: Address) {
        if let Err(err) = self
            .tx
            .send(UiEvent::CallAccepted {
                address: address.to_string(),
            })
            .await
        {
            tracing::error!(?err, "Cannot send UI event: CallAccepted");
        }
    }

    async fn on_call_rejected(&self, address: Address) {
        if let Err(err) = self
            .tx
            .send(UiEvent::CallRejected {
                address: address.to_string(),
            })
            .await
        {
            tracing::error!(?err, "Cannot send UI event: CallRejected");
        }
    }

    async fn on_call_connected(&self, address: Address, is_muted: bool) {
        if let Err(err) = self
            .tx
            .send(UiEvent::CallConnected {
                address: address.to_string(),
                is_muted,
            })
            .await
        {
            tracing::error!(?err, "Cannot send UI event: CallConnected");
        }
    }

    async fn on_call_ended(&self, address: Address, reason: &str) {
        if let Err(err) = self
            .tx
            .send(UiEvent::CallEnded {
                address: address.to_string(),
                reason: reason.to_string(),
            })
            .await
        {
            tracing::error!(?err, "Cannot send UI event: CallEnded");
        }
    }

    async fn on_call_state_changed(&self, address: Address, state: &str) {
        if let Err(err) = self
            .tx
            .send(UiEvent::CallStateChanged {
                address: address.to_string(),
                state: state.to_string(),
            })
            .await
        {
            tracing::error!(?err, "Cannot send UI event: CallStateChanged");
        }
    }

    async fn on_audio_data_received(&self, _address: Address, _data: Vec<u8>) {
        // TODO: Play audio data
    }

    async fn on_video_frame_received(&self, _address: Address, _frame: Vec<u8>) {
        // TODO: Display video frame
    }
}

#[async_trait]
impl ChatListener for UiEventListener {
    async fn on_incoming_message(&self, address: Address, message: Message) {
        let text = match message.kind {
            MessageKind::Text(s) => s,
        };
        if let Err(err) = self
            .tx
            .send(UiEvent::NewMessage {
                id: message.id,
                address: address.to_string(),
                incoming: true,
                text,
            })
            .await
        {
            tracing::error!(?err, "Cannot send UI event: NewMessage incoming");
        }
    }

    async fn on_outgoing_message(&self, address: Address, message: Message) {
        if let Err(err) = self
            .tx
            .send(UiEvent::MessageDelivered {
                id: message.id,
                address: address.to_string(),
            })
            .await
        {
            tracing::error!(?err, "Cannot send UI event: MessageDelivered");
        }
    }
}
