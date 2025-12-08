use async_trait::async_trait;
use ntied_crypto::PublicKey;
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
        public_key: String,
    },
    OutgoingRequest {
        public_key: String,
    },
    ContactAccepted {
        name: String,
        public_key: String,
    },
    ContactRemoved {
        public_key: String,
    },
    ContactConnection {
        public_key: String,
        connected: bool,
    },
    NewMessage {
        id: i64,
        public_key: String,
        incoming: bool,
        text: String,
    },
    MessageSent {
        id: i64,
        public_key: String,
        text: String,
    },
    MessageDelivered {
        id: i64,
        public_key: String,
    },
    // Call events
    IncomingCall {
        public_key: String,
    },
    OutgoingCall {
        public_key: String,
    },
    CallAccepted {
        public_key: String,
    },
    CallRejected {
        public_key: String,
    },
    CallConnected {
        public_key: String,
    },
    CallEnded {
        public_key: String,
        reason: String,
    },
    CallStateChanged {
        public_key: String,
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

    async fn on_contact_connected(&self, public_key: PublicKey) {
        if let Err(err) = self
            .tx
            .send(UiEvent::ContactConnection {
                public_key: public_key.to_string(),
                connected: true,
            })
            .await
        {
            tracing::error!(?err, "Cannot send UI event: ContactConnection");
        }
    }

    async fn on_contact_disconnected(&self, public_key: PublicKey) {
        if let Err(err) = self
            .tx
            .send(UiEvent::ContactConnection {
                public_key: public_key.to_string(),
                connected: false,
            })
            .await
        {
            tracing::error!(?err, "Cannot send UI event: ContactConnection");
        }
    }

    async fn on_contact_incoming(&self, public_key: PublicKey, profile: ContactProfile) {
        if let Err(err) = self
            .tx
            .send(UiEvent::IncomingRequest {
                name: profile.name,
                public_key: public_key.to_string(),
            })
            .await
        {
            tracing::error!(?err, "Cannot send UI event: IncomingRequest");
        }
    }

    async fn on_contact_accepted(&self, public_key: PublicKey, profile: ContactProfile) {
        if let Err(err) = self
            .tx
            .send(UiEvent::ContactAccepted {
                name: profile.name,
                public_key: public_key.to_string(),
            })
            .await
        {
            tracing::error!(?err, "Cannot send UI event: ContactAccepted");
        }
    }

    async fn on_contact_rejected(&self, public_key: PublicKey) {
        if let Err(err) = self
            .tx
            .send(UiEvent::ContactRemoved {
                public_key: public_key.to_string(),
            })
            .await
        {
            tracing::error!(?err, "Cannot send UI event: ContactRemoved");
        }
    }
}

#[async_trait]
impl CallListener for UiEventListener {
    async fn on_incoming_call(&self, public_key: PublicKey) {
        if let Err(err) = self
            .tx
            .send(UiEvent::IncomingCall {
                public_key: public_key.to_string(),
            })
            .await
        {
            tracing::error!(?err, "Cannot send UI event: IncomingCall");
        }
    }

    async fn on_outgoing_call(&self, public_key: PublicKey) {
        if let Err(err) = self
            .tx
            .send(UiEvent::OutgoingCall {
                public_key: public_key.to_string(),
            })
            .await
        {
            tracing::error!(?err, "Cannot send UI event: OutgoingCall");
        }
    }

    async fn on_call_accepted(&self, public_key: PublicKey) {
        if let Err(err) = self
            .tx
            .send(UiEvent::CallAccepted {
                public_key: public_key.to_string(),
            })
            .await
        {
            tracing::error!(?err, "Cannot send UI event: CallAccepted");
        }
    }

    async fn on_call_rejected(&self, public_key: PublicKey) {
        if let Err(err) = self
            .tx
            .send(UiEvent::CallRejected {
                public_key: public_key.to_string(),
            })
            .await
        {
            tracing::error!(?err, "Cannot send UI event: CallRejected");
        }
    }

    async fn on_call_connected(&self, public_key: PublicKey) {
        if let Err(err) = self
            .tx
            .send(UiEvent::CallConnected {
                public_key: public_key.to_string(),
            })
            .await
        {
            tracing::error!(?err, "Cannot send UI event: CallConnected");
        }
    }

    async fn on_call_ended(&self, public_key: PublicKey, reason: &str) {
        if let Err(err) = self
            .tx
            .send(UiEvent::CallEnded {
                public_key: public_key.to_string(),
                reason: reason.to_string(),
            })
            .await
        {
            tracing::error!(?err, "Cannot send UI event: CallEnded");
        }
    }

    async fn on_call_state_changed(&self, public_key: PublicKey, state: &str) {
        if let Err(err) = self
            .tx
            .send(UiEvent::CallStateChanged {
                public_key: public_key.to_string(),
                state: state.to_string(),
            })
            .await
        {
            tracing::error!(?err, "Cannot send UI event: CallStateChanged");
        }
    }

    async fn on_audio_data_received(&self, _public_key: PublicKey, _data: Vec<u8>) {
        // TODO: Play audio data
    }

    async fn on_video_frame_received(&self, _public_key: PublicKey, _frame: Vec<u8>) {
        // TODO: Display video frame
    }
}

#[async_trait]
impl ChatListener for UiEventListener {
    async fn on_incoming_message(&self, public_key: PublicKey, message: Message) {
        let text = match message.kind {
            MessageKind::Text(s) => s,
        };
        if let Err(err) = self
            .tx
            .send(UiEvent::NewMessage {
                id: message.id,
                public_key: public_key.to_string(),
                incoming: true,
                text,
            })
            .await
        {
            tracing::error!(?err, "Cannot send UI event: NewMessage incoming");
        }
    }

    async fn on_outgoing_message(&self, public_key: PublicKey, message: Message) {
        if let Err(err) = self
            .tx
            .send(UiEvent::MessageDelivered {
                id: message.id,
                public_key: public_key.to_string(),
            })
            .await
        {
            tracing::error!(?err, "Cannot send UI event: MessageDelivered");
        }
    }
}
