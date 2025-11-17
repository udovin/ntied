use std::collections::HashMap;

use iced::widget::{
    Space, button, column, container, row, scrollable, slider, stack, svg, text, text_input,
};
use iced::{Alignment, Color, Element, Length, Padding, Task, Theme, clipboard};

use crate::ui::core::{Screen, ScreenCommand, ScreenType};
use crate::ui::theme::{colors, styles};
use crate::ui::{AppContext, UiEvent};

// SVG Icons
const COPY_ICON: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="currentColor">
    <path d="M16 1H4c-1.1 0-2 .9-2 2v14h2V3h12V1zm3 4H8c-1.1 0-2 .9-2 2v14c0 1.1.9 2 2 2h11c1.1 0 2-.9 2-2V7c0-1.1-.9-2-2-2zm0 16H8V7h11v14z"/>
</svg>"#;

const ADD_ICON: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="currentColor">
    <path d="M19 13h-6v6h-2v-6H5v-2h6V5h2v6h6v2z"/>
</svg>"#;

const SETTINGS_ICON: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="currentColor">
    <path d="M12 15.5A3.5 3.5 0 0 1 8.5 12 3.5 3.5 0 0 1 12 8.5a3.5 3.5 0 0 1 3.5 3.5 3.5 3.5 0 0 1-3.5 3.5m7.43-2.53c.04-.32.07-.64.07-.97 0-.33-.03-.66-.07-1l2.11-1.63c.19-.15.24-.42.12-.64l-2-3.46c-.12-.22-.39-.3-.61-.22l-2.49 1c-.52-.39-1.06-.73-1.69-.98l-.37-2.65A.506.506 0 0 0 14 2h-4c-.25 0-.46.18-.5.42l-.37 2.65c-.63.25-1.17.59-1.69.98l-2.49-1c-.22-.09-.49 0-.61.22l-2 3.46c-.13.22-.07.49.12.64L4.57 11c-.04.34-.07.67-.07 1 0 .33.03.65.07.97l-2.11 1.66c-.19.15-.25.42-.12.64l2 3.46c.12.22.39.3.61.22l2.49-1.01c.52.4 1.06.74 1.69.99l.37 2.65c.04.24.25.42.5.42h4c.25 0 .46-.18.5-.42l.37-2.65c.63-.26 1.17-.59 1.69-.99l2.49 1.01c.22.08.49 0 .61-.22l2-3.46c.12-.22.07-.49-.12-.64l-2.11-1.66Z"/>
</svg>"#;

const SEND_ICON: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="currentColor">
    <path d="M2.01 21L23 12 2.01 3 2 10l15 2-15 2z"/>
</svg>"#;

const PHONE_CALL_ICON: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="currentColor">
    <path d="M20.01 15.38c-1.23 0-2.42-.2-3.53-.56a.977.977 0 0 0-1.01.24l-1.57 1.97c-2.83-1.35-5.48-3.9-6.89-6.83l1.95-1.66c.27-.28.35-.67.24-1.02-.37-1.11-.56-2.3-.56-3.53 0-.54-.45-.99-.99-.99H4.19C3.65 3 3 3.24 3 3.99 3 13.28 10.73 21 20.01 21c.71 0 .99-.63.99-1.18v-3.45c0-.54-.45-.99-.99-.99z"/>
</svg>"#;

const MIC_SETTINGS_ICON: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="currentColor">
    <path d="M12 14c1.66 0 3-1.34 3-3V5c0-1.66-1.34-3-3-3S9 3.34 9 5v6c0 1.66 1.34 3 3 3zm5.91-3c-.49 0-.9.36-.98.85C16.52 14.2 14.47 16 12 16s-4.52-1.8-4.93-4.15c-.08-.49-.49-.85-.98-.85-.61 0-1.09.54-1 1.14.49 3 2.89 5.35 5.91 5.78V20c0 .55.45 1 1 1s1-.45 1-1v-2.08c3.02-.43 5.42-2.78 5.91-5.78.1-.6-.39-1.14-1-1.14z"/>
</svg>"#;

const SPEAKER_ICON: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="currentColor">
    <path d="M3 9v6h4l5 5V4L7 9H3zm13.5 3c0-1.77-1.02-3.29-2.5-4.03v8.05c1.48-.73 2.5-2.25 2.5-4.02zM14 3.23v2.06c2.89.86 5 3.54 5 6.71s-2.11 5.85-5 6.71v2.06c4.01-.91 7-4.49 7-8.77s-2.99-7.86-7-8.77z"/>
</svg>"#;

const MIC_ON_ICON: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="currentColor">
    <path d="M12 14c1.66 0 3-1.34 3-3V5c0-1.66-1.34-3-3-3S9 3.34 9 5v6c0 1.66 1.34 3 3 3z"/>
    <path d="M17 11c0 2.76-2.24 5-5 5s-5-2.24-5-5H5c0 3.53 2.61 6.43 6 6.92V21h2v-3.08c3.39-.49 6-3.39 6-6.92h-2z"/>
</svg>"#;

const MIC_OFF_ICON: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="currentColor">
    <path d="m19 11h-1.7c0 .74-.16 1.43-.43 2.05l1.23 1.23c.56-.98.9-2.09.9-3.28zm-4.02.17c0-.06.02-.11.02-.17V5c0-1.66-1.34-3-3-3S9 3.34 9 5v.18l5.98 5.99zM4.27 3L3 4.27l6.01 6.01V11c0 1.66 1.33 3 2.99 3 .22 0 .44-.03.65-.08l1.66 1.66c-.71.33-1.5.52-2.31.52-2.76 0-5.3-2.1-5.3-5.1H5c0 3.41 2.72 6.23 6 6.72V21h2v-3.28c.91-.13 1.77-.45 2.54-.9L19.73 21 21 19.73 4.27 3z"/>
</svg>"#;

#[derive(Clone, Debug)]
pub enum ChatListMessage {
    SelectChat(String),
    CopyOwnAddress,
    CopyPeerAddress(String),
    AcceptIncoming(String),
    RejectIncoming(String),
    CancelOutgoing(String),
    ShowAddContactModal,
    HideAddContactModal,
    AddContactInputChanged(String),
    AddContactSubmit,
    ComposeChanged(String),
    SendMessage,
    OpenSettings,
    Logout,
    ClearError,
    // Call messages
    StartVoiceCall(String),
    AcceptCall(String),
    RejectCall(String),
    HangupCall(String),
    ToggleMute,
    ShowAudioSettings,
    HideAudioSettings,
    SelectInputDevice(String),
    SelectOutputDevice(String),
    SpeakerVolumeChanged(f32),
    MicrophoneVolumeChanged(f32),
    DevicesLoaded(Vec<(String, bool)>, Vec<(String, bool)>), // (name, is_default)
    DevicesLoadedWithCurrent(
        Vec<(String, bool)>, // input devices
        Vec<(String, bool)>, // output devices
        Option<String>,      // currently selected input
        Option<String>,      // currently selected output
    ),
    // Async operation results
    CallOperationComplete(Result<(), String>),
    ContactOperationComplete(Result<(), String>),
    MessageSent(Result<i64, String>),
    DeviceSwitchComplete(Result<(), String>),
    // State synchronization messages
    SyncCallState, // Request to sync state with CallManager
    CallStateSynced {
        is_muted: bool,
        capture_volume: f32,
        playback_volume: f32,
        input_device: Option<String>,
        output_device: Option<String>,
    },
    MuteToggled(bool), // Result of toggle_mute operation
    Noop,              // For operations that don't need result handling
}

#[derive(Clone, Debug)]
struct PendingIncoming {
    name: String,
    address: String,
}

#[derive(Clone, Debug)]
struct PendingOutgoing {
    address: String,
}

#[derive(Clone, Debug)]
struct ContactSummary {
    name: String,
    address: String,
    connected: bool,
    last_message: Option<String>,
}

#[derive(Clone, Debug)]
struct CallInfo {
    address: String,
    name: String,
    state: CallState,
}

#[derive(Clone, Debug)]
struct IncomingCallInfo {
    address: String,
    name: String,
}

#[derive(Clone, Debug, PartialEq)]
enum CallState {
    Calling,
    Ringing,
    Connected,
}

#[derive(Clone, Debug)]
struct MessageItem {
    id: i64,
    text: String,
    is_mine: bool,
    delivered: bool,
    timestamp: String,
}

pub struct ChatListScreen {
    own_name: String,
    own_address: String,
    transport_connected: bool,
    incoming_pending: Vec<PendingIncoming>,
    outgoing_pending: Vec<PendingOutgoing>,
    contacts: Vec<ContactSummary>,
    selected_chat: Option<String>,
    messages_by_addr: HashMap<String, Vec<MessageItem>>,
    show_add_contact_modal: bool,
    add_contact_addr: String,
    add_contact_error: Option<String>,
    compose_text: String,
    global_error: Option<String>,
    should_scroll_to_end: bool,
    messages_scrollable_id: scrollable::Id,
    // Call state
    active_call: Option<CallInfo>,
    incoming_call: Option<IncomingCallInfo>,

    // Audio settings
    show_audio_settings: bool,
    is_muted: bool,
    available_input_devices: Vec<String>,
    available_output_devices: Vec<String>,
    selected_input_device: Option<String>,
    selected_output_device: Option<String>,
    speaker_volume: f32,    // 0.0 to 2.0, default 1.0 (100%)
    microphone_volume: f32, // 0.0 to 2.0, default 1.0 (100%)
}

impl ChatListScreen {
    pub fn new(profile_name: Option<String>) -> Self {
        Self {
            own_name: profile_name.unwrap_or_else(|| "Me".to_string()),
            own_address: String::new(),
            transport_connected: false,
            incoming_pending: Vec::new(),
            outgoing_pending: Vec::new(),
            contacts: Vec::new(),
            selected_chat: None,
            messages_by_addr: HashMap::new(),
            show_add_contact_modal: false,
            add_contact_addr: String::new(),
            add_contact_error: None,
            compose_text: String::new(),
            global_error: None,
            should_scroll_to_end: false,
            messages_scrollable_id: scrollable::Id::unique(),
            active_call: None,
            incoming_call: None,
            show_audio_settings: false,
            is_muted: false,
            available_input_devices: Vec::new(),
            available_output_devices: Vec::new(),
            selected_input_device: None,
            selected_output_device: None,
            speaker_volume: 1.0,
            microphone_volume: 1.0,
        }
    }

    pub fn set_identity(&mut self, name: String, address: String) {
        self.own_name = name;
        self.own_address = address;
    }

    pub fn set_error(&mut self, msg: impl Into<String>) {
        self.global_error = Some(msg.into());
    }

    pub fn message_draft(&self) -> &str {
        &self.compose_text
    }

    // Methods to save/restore call state for preservation across screen switches
    pub fn get_active_call_address(&self) -> Option<String> {
        self.active_call.as_ref().map(|c| c.address.clone())
    }

    pub fn get_active_call_name(&self) -> Option<String> {
        self.active_call.as_ref().map(|c| c.name.clone())
    }

    pub fn get_active_call_state(&self) -> Option<String> {
        self.active_call.as_ref().map(|c| match c.state {
            CallState::Calling => "calling".to_string(),
            CallState::Ringing => "ringing".to_string(),
            CallState::Connected => "connected".to_string(),
        })
    }

    pub fn get_incoming_call_address(&self) -> Option<String> {
        self.incoming_call.as_ref().map(|c| c.address.clone())
    }

    pub fn get_incoming_call_name(&self) -> Option<String> {
        self.incoming_call.as_ref().map(|c| c.name.clone())
    }

    pub fn restore_call_state(
        &mut self,
        active_call_address: Option<String>,
        active_call_name: Option<String>,
        active_call_state: Option<String>,
        incoming_call_address: Option<String>,
        incoming_call_name: Option<String>,
    ) {
        // Restore active call if exists
        if let (Some(address), Some(name), Some(state_str)) =
            (active_call_address, active_call_name, active_call_state)
        {
            let state = match state_str.as_str() {
                "calling" => CallState::Calling,
                "ringing" => CallState::Ringing,
                "connected" => CallState::Connected,
                _ => CallState::Calling,
            };
            self.active_call = Some(CallInfo {
                address,
                name,
                state,
            });
        }

        // Restore incoming call if exists
        if let (Some(address), Some(name)) = (incoming_call_address, incoming_call_name) {
            self.incoming_call = Some(IncomingCallInfo { address, name });
        }
    }

    pub fn apply_event(&mut self, event: UiEvent) {
        match event {
            UiEvent::TransportConnected(connected) => {
                self.transport_connected = connected;
            }

            UiEvent::IncomingRequest { name, address } => {
                if !self.incoming_pending.iter().any(|p| p.address == address) {
                    self.incoming_pending
                        .push(PendingIncoming { name, address });
                }
            }

            UiEvent::OutgoingRequest { address } => {
                if !self.outgoing_pending.iter().any(|p| p.address == address) {
                    self.outgoing_pending.push(PendingOutgoing { address });
                }
            }

            UiEvent::ContactAccepted { address, name } => {
                self.incoming_pending.retain(|p| p.address != address);
                self.outgoing_pending.retain(|p| p.address != address);
                if !self.contacts.iter().any(|c| c.address == address) {
                    self.contacts.push(ContactSummary {
                        name,
                        address: address.clone(),
                        connected: true,
                        last_message: None,
                    });
                }
            }
            UiEvent::ContactRemoved { address } => {
                self.incoming_pending.retain(|p| p.address != address);
                self.outgoing_pending.retain(|p| p.address != address);
                self.contacts.retain(|c| c.address != address);
                self.messages_by_addr.remove(&address);
                if self
                    .selected_chat
                    .as_ref()
                    .map(|a| a == &address)
                    .unwrap_or(false)
                {
                    self.selected_chat = None;
                }
            }

            UiEvent::ContactConnection { address, connected } => {
                if let Some(c) = self.contacts.iter_mut().find(|c| c.address == address) {
                    c.connected = connected;
                }
            }

            UiEvent::NewMessage {
                id,
                address,
                incoming,
                text,
            } => {
                let entry = self.messages_by_addr.entry(address.clone()).or_default();
                if let Some(pos) = entry.iter_mut().position(|m| m.id == id) {
                    entry[pos] = MessageItem {
                        id,
                        text: text.clone(),
                        is_mine: !incoming,
                        delivered: true,
                        timestamp: "12:34".to_string(),
                    }
                } else {
                    entry.push(MessageItem {
                        id,
                        text: text.clone(),
                        is_mine: !incoming,
                        delivered: true,
                        timestamp: "12:34".to_string(),
                    });
                }
                if let Some(c) = self.contacts.iter_mut().find(|c| c.address == address) {
                    c.last_message = Some(text);
                }
                if self
                    .selected_chat
                    .as_ref()
                    .map(|a| a == &address)
                    .unwrap_or(false)
                {
                    self.should_scroll_to_end = true;
                }
            }
            UiEvent::MessageSent { id, address, text } => {
                let entry = self.messages_by_addr.entry(address.clone()).or_default();
                if entry.iter_mut().position(|m| m.id == id).is_none() {
                    entry.push(MessageItem {
                        id: id,
                        text: text.clone(),
                        is_mine: true,
                        delivered: false,
                        timestamp: "12:34".to_string(),
                    });
                    if let Some(c) = self.contacts.iter_mut().find(|c| c.address == address) {
                        c.last_message = Some(text);
                    }
                }
                if self
                    .selected_chat
                    .as_ref()
                    .map(|a| a == &address)
                    .unwrap_or(false)
                {
                    self.should_scroll_to_end = true;
                }
            }
            UiEvent::MessageDelivered { id, address } => {
                if let Some(list) = self.messages_by_addr.get_mut(&address) {
                    if let Some(pos) = list.iter_mut().position(|m| m.id == id) {
                        list[pos].delivered = true;
                    }
                }
                if self
                    .selected_chat
                    .as_ref()
                    .map(|a| a == &address)
                    .unwrap_or(false)
                {
                    self.should_scroll_to_end = true;
                }
            }

            // Call events
            UiEvent::IncomingCall { address } => {
                // Find contact name
                let name = self
                    .contacts
                    .iter()
                    .find(|c| c.address == address)
                    .map(|c| c.name.clone())
                    .unwrap_or_else(|| address.clone());

                self.incoming_call = Some(IncomingCallInfo { address, name });
            }

            UiEvent::OutgoingCall { address } => {
                let name = self
                    .contacts
                    .iter()
                    .find(|c| c.address == address)
                    .map(|c| c.name.clone())
                    .unwrap_or_else(|| address.clone());

                // Clear any incoming call and show outgoing call immediately
                self.incoming_call = None;
                self.active_call = Some(CallInfo {
                    address: address.clone(),
                    name,
                    state: CallState::Calling,
                });
            }

            UiEvent::CallAccepted { address } => {
                if let Some(call) = &mut self.active_call {
                    if call.address == address {
                        call.state = CallState::Connected;
                    }
                }
            }

            UiEvent::CallRejected { address } => {
                if self
                    .active_call
                    .as_ref()
                    .map(|c| c.address == address)
                    .unwrap_or(false)
                {
                    self.active_call = None;
                }
                if self
                    .incoming_call
                    .as_ref()
                    .map(|c| c.address == address)
                    .unwrap_or(false)
                {
                    self.incoming_call = None;
                }
            }

            UiEvent::CallConnected { address } => {
                // Clear incoming call if this was an accepted incoming call
                if self
                    .incoming_call
                    .as_ref()
                    .map(|c| c.address == address)
                    .unwrap_or(false)
                {
                    let incoming = self.incoming_call.take().unwrap();
                    self.active_call = Some(CallInfo {
                        address: incoming.address.clone(),
                        name: incoming.name.clone(),
                        state: CallState::Connected,
                    });
                } else if let Some(call) = &mut self.active_call {
                    // Update existing active call to connected
                    if call.address == address {
                        call.state = CallState::Connected;
                    }
                }
            }

            UiEvent::CallEnded { address, reason: _ } => {
                if self
                    .active_call
                    .as_ref()
                    .map(|c| c.address == address)
                    .unwrap_or(false)
                {
                    self.active_call = None;
                    self.speaker_volume = 1.0;
                    self.microphone_volume = 1.0;
                    self.is_muted = false;
                }
                if self
                    .incoming_call
                    .as_ref()
                    .map(|c| c.address == address)
                    .unwrap_or(false)
                {
                    self.incoming_call = None;
                    self.speaker_volume = 1.0;
                    self.microphone_volume = 1.0;
                    self.is_muted = false;
                }
            }

            UiEvent::CallStateChanged {
                address: _,
                state: _,
            } => {
                // Handle state changes if needed
            }
        }
    }

    fn update_internal(&mut self, message: ChatListMessage) -> Task<ChatListMessage> {
        match message {
            ChatListMessage::SelectChat(addr) => {
                self.selected_chat = Some(addr.clone());
                self.should_scroll_to_end = true;
                // Clear message composition when switching chats
                self.compose_text.clear();
                // Trigger scroll to bottom
                scrollable::snap_to(
                    self.messages_scrollable_id.clone(),
                    scrollable::RelativeOffset::END,
                )
            }
            ChatListMessage::CopyOwnAddress => clipboard::write(self.own_address.clone()),
            ChatListMessage::CopyPeerAddress(addr) => clipboard::write(addr),
            ChatListMessage::AcceptIncoming(addr) => {
                self.incoming_pending.retain(|p| p.address != addr);
                Task::none()
            }
            ChatListMessage::RejectIncoming(addr) => {
                self.incoming_pending.retain(|p| p.address != addr);
                Task::none()
            }
            ChatListMessage::CancelOutgoing(addr) => {
                self.outgoing_pending.retain(|p| p.address != addr);
                Task::none()
            }
            ChatListMessage::ShowAddContactModal => {
                self.show_add_contact_modal = true;
                self.add_contact_addr.clear();
                self.add_contact_error = None;
                Task::none()
            }
            ChatListMessage::HideAddContactModal => {
                self.show_add_contact_modal = false;
                Task::none()
            }
            ChatListMessage::AddContactInputChanged(value) => {
                self.add_contact_addr = value;
                self.add_contact_error = Self::validate_address(&self.add_contact_addr);
                self.global_error = None;
                Task::none()
            }
            ChatListMessage::AddContactSubmit => {
                self.add_contact_error = Self::validate_address(&self.add_contact_addr);
                if self.add_contact_error.is_none() {
                    let addr = self.add_contact_addr.trim().to_string();
                    if !addr.is_empty() && !self.outgoing_pending.iter().any(|p| p.address == addr)
                    {
                        self.outgoing_pending
                            .push(PendingOutgoing { address: addr });
                    }
                    self.add_contact_addr.clear();
                    self.show_add_contact_modal = false;
                }
                Task::none()
            }
            ChatListMessage::ComposeChanged(value) => {
                self.compose_text = value;
                Task::none()
            }
            ChatListMessage::SendMessage => {
                if self.selected_chat.is_some() {
                    let text = self.compose_text.trim().to_string();
                    if !text.is_empty() {
                        // Clear the compose text first
                        self.compose_text.clear();
                        // Don't add to local state here - let the event system handle it
                        self.should_scroll_to_end = true;
                        // Parent component should handle actual sending
                        // Return command to trigger scroll
                        return scrollable::snap_to(
                            self.messages_scrollable_id.clone(),
                            scrollable::RelativeOffset::END,
                        );
                    }
                }
                Task::none()
            }
            ChatListMessage::OpenSettings => {
                // Settings functionality to be implemented
                Task::none()
            }
            ChatListMessage::Logout => Task::none(),
            ChatListMessage::ClearError => {
                self.global_error = None;
                Task::none()
            }
            // Call messages - these will be handled by the parent app
            ChatListMessage::StartVoiceCall(_addr) => {
                // Don't update UI state here - wait for OutgoingCall event from backend
                Task::none()
            }
            ChatListMessage::AcceptCall(addr) => {
                // When accepting, transition to "Connecting" state while waiting for backend
                if let Some(incoming) = &self.incoming_call {
                    if incoming.address == addr {
                        self.active_call = Some(CallInfo {
                            address: incoming.address.clone(),
                            name: incoming.name.clone(),
                            state: CallState::Ringing, // Keep in Ringing until CallConnected event
                        });
                        // Don't clear incoming_call yet - wait for CallConnected event
                    }
                }
                Task::none()
            }
            ChatListMessage::RejectCall(addr) => {
                // Clear incoming call
                if self
                    .incoming_call
                    .as_ref()
                    .map(|c| c.address == addr)
                    .unwrap_or(false)
                {
                    self.incoming_call = None;
                    self.speaker_volume = 1.0;
                    self.microphone_volume = 1.0;
                    self.is_muted = false;
                }
                Task::none()
            }
            ChatListMessage::HangupCall(addr) => {
                // Clear active call
                if self
                    .active_call
                    .as_ref()
                    .map(|c| c.address == addr)
                    .unwrap_or(false)
                {
                    self.active_call = None;
                    self.speaker_volume = 1.0;
                    self.microphone_volume = 1.0;
                    self.is_muted = false;
                }
                Task::none()
            }
            ChatListMessage::ToggleMute => {
                // Don't update state here - wait for MuteToggled message from CallManager
                Task::none()
            }
            ChatListMessage::MuteToggled(is_muted) => {
                // Update state based on actual CallManager state
                self.is_muted = is_muted;
                Task::none()
            }
            ChatListMessage::ShowAudioSettings => {
                self.show_audio_settings = true;
                // Load audio devices when opening settings
                // Keep the currently selected devices if they exist
                let keep_current_input = self.selected_input_device.clone();
                let keep_current_output = self.selected_output_device.clone();
                Task::perform(
                    async move {
                        let input_devices = crate::audio::AudioManager::list_input_devices()
                            .await
                            .unwrap_or_default()
                            .into_iter()
                            .map(|d| (d.name, d.is_default))
                            .collect::<Vec<_>>();
                        let output_devices = crate::audio::AudioManager::list_output_devices()
                            .await
                            .unwrap_or_default()
                            .into_iter()
                            .map(|d| (d.name, d.is_default))
                            .collect::<Vec<_>>();
                        (
                            input_devices,
                            output_devices,
                            keep_current_input,
                            keep_current_output,
                        )
                    },
                    |(input, output, current_input, current_output)| {
                        ChatListMessage::DevicesLoadedWithCurrent(
                            input,
                            output,
                            current_input,
                            current_output,
                        )
                    },
                )
            }
            ChatListMessage::HideAudioSettings => {
                self.show_audio_settings = false;
                Task::none()
            }
            ChatListMessage::SelectInputDevice(device) => {
                // Update UI immediately to show selection
                self.selected_input_device = Some(device.clone());
                // The actual device switch happens in the parent app layer
                Task::none()
            }
            ChatListMessage::SelectOutputDevice(device) => {
                // Update UI immediately to show selection
                self.selected_output_device = Some(device.clone());
                // The actual device switch happens in the parent app layer
                Task::none()
            }
            ChatListMessage::SpeakerVolumeChanged(volume) => {
                self.speaker_volume = volume;
                Task::none()
            }
            ChatListMessage::MicrophoneVolumeChanged(volume) => {
                self.microphone_volume = volume;
                Task::none()
            }
            ChatListMessage::DevicesLoaded(input_devices, output_devices) => {
                // Extract device names and find defaults
                self.available_input_devices =
                    input_devices.iter().map(|(name, _)| name.clone()).collect();
                self.available_output_devices = output_devices
                    .iter()
                    .map(|(name, _)| name.clone())
                    .collect();

                // Select default devices if none selected
                if self.selected_input_device.is_none() {
                    // Try to find the default device, otherwise use first
                    self.selected_input_device = input_devices
                        .iter()
                        .find(|(_, is_default)| *is_default)
                        .map(|(name, _)| name.clone())
                        .or_else(|| self.available_input_devices.first().cloned());
                }
                if self.selected_output_device.is_none() {
                    // Try to find the default device, otherwise use first
                    self.selected_output_device = output_devices
                        .iter()
                        .find(|(_, is_default)| *is_default)
                        .map(|(name, _)| name.clone())
                        .or_else(|| self.available_output_devices.first().cloned());
                }
                Task::none()
            }
            ChatListMessage::DevicesLoadedWithCurrent(
                input_devices,
                output_devices,
                current_input,
                current_output,
            ) => {
                // Extract device names
                self.available_input_devices =
                    input_devices.iter().map(|(name, _)| name.clone()).collect();
                self.available_output_devices = output_devices
                    .iter()
                    .map(|(name, _)| name.clone())
                    .collect();

                // Use the current selection if it exists and is in the list
                if let Some(current) = current_input {
                    if self.available_input_devices.contains(&current) {
                        self.selected_input_device = Some(current);
                    } else {
                        // Fall back to default if current device is not available
                        self.selected_input_device = input_devices
                            .iter()
                            .find(|(_, is_default)| *is_default)
                            .map(|(name, _)| name.clone())
                            .or_else(|| self.available_input_devices.first().cloned());
                    }
                } else {
                    // No current selection, use default
                    self.selected_input_device = input_devices
                        .iter()
                        .find(|(_, is_default)| *is_default)
                        .map(|(name, _)| name.clone())
                        .or_else(|| self.available_input_devices.first().cloned());
                }

                // Same for output devices
                if let Some(current) = current_output {
                    if self.available_output_devices.contains(&current) {
                        self.selected_output_device = Some(current);
                    } else {
                        self.selected_output_device = output_devices
                            .iter()
                            .find(|(_, is_default)| *is_default)
                            .map(|(name, _)| name.clone())
                            .or_else(|| self.available_output_devices.first().cloned());
                    }
                } else {
                    self.selected_output_device = output_devices
                        .iter()
                        .find(|(_, is_default)| *is_default)
                        .map(|(name, _)| name.clone())
                        .or_else(|| self.available_output_devices.first().cloned());
                }
                Task::none()
            }
            // Handle async operation results
            ChatListMessage::CallOperationComplete(_) => Task::none(),
            ChatListMessage::ContactOperationComplete(_) => Task::none(),
            ChatListMessage::MessageSent(_) => Task::none(),
            ChatListMessage::DeviceSwitchComplete(_) => Task::none(),
            // State synchronization
            ChatListMessage::SyncCallState => {
                // This message is handled at the parent level (app.rs)
                // It triggers an async Task to query CallManager
                Task::none()
            }
            ChatListMessage::CallStateSynced {
                is_muted,
                capture_volume,
                playback_volume,
                input_device,
                output_device,
            } => {
                // Update UI state with actual values from CallManager
                self.is_muted = is_muted;
                self.microphone_volume = capture_volume;
                self.speaker_volume = playback_volume;
                self.selected_input_device = input_device;
                self.selected_output_device = output_device;
                tracing::debug!(
                    "Call state synced: muted={}, capture_vol={:.2}, playback_vol={:.2}",
                    is_muted,
                    capture_volume,
                    playback_volume
                );
                Task::none()
            }
            ChatListMessage::Noop => Task::none(),
        }
    }

    pub fn view<'a>(&'a self, theme: &'a Theme) -> Element<'a, ChatListMessage> {
        let left_panel = self.build_left_panel(theme);
        let right_panel = self.build_right_panel(theme);
        let divider = container(Space::with_width(1))
            .height(Length::Fill)
            .style(move |t: &Theme| styles::divider(t));

        let main_content = container(
            row![left_panel, divider, right_panel]
                .spacing(0)
                .align_y(Alignment::Start),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |t: &Theme| container::Style {
            background: Some(iced::Background::Color(colors::background_base(t))),
            ..Default::default()
        });

        // Check for active call first (highest priority)
        let main_element: Element<'a, ChatListMessage> = main_content.into();

        if let Some(call) = &self.active_call {
            let call_overlay = self.build_active_call_overlay(call.clone(), main_element, theme);
            // Check if we need to show add contact modal on top of call overlay
            if self.show_add_contact_modal {
                let modal = self.build_add_contact_modal(theme);
                return stack![call_overlay, modal].into();
            }
            return call_overlay;
        }

        // Then check for incoming call
        if let Some(incoming) = &self.incoming_call {
            let incoming_overlay =
                self.build_incoming_call_overlay(incoming.clone(), main_element, theme);
            // Check if we need to show add contact modal on top of incoming call overlay
            if self.show_add_contact_modal {
                let modal = self.build_add_contact_modal(theme);
                return stack![incoming_overlay, modal].into();
            }
            return incoming_overlay;
        }

        // Use stack to properly layer modal over main content
        if self.show_add_contact_modal {
            let modal = self.build_add_contact_modal(theme);
            stack![main_element, modal].into()
        } else {
            main_element
        }
    }

    fn build_left_panel(&self, theme: &Theme) -> Element<'_, ChatListMessage> {
        // Connection status circle
        let transport_connected = self.transport_connected;
        let status_circle = container(Space::new(12, 12)).style(move |t: &Theme| {
            if transport_connected {
                styles::status_connected(t)
            } else {
                styles::status_disconnected(t)
            }
        });

        // Account name with truncation
        let name_container = container(
            text(&self.own_name)
                .size(18)
                .color(colors::text_primary(theme)),
        )
        .width(Length::Fill);

        let name_row = row![name_container, status_circle]
            .align_y(Alignment::Center)
            .spacing(8);

        let icon_color = colors::text_primary(theme);
        let copy_icon = svg::Svg::new(svg::Handle::from_memory(COPY_ICON.as_bytes().to_vec()))
            .width(Length::Fixed(16.0))
            .height(Length::Fixed(16.0))
            .style(move |_theme, _status| svg::Style {
                color: Some(icon_color),
            });

        let addr_text = container(
            text(&self.own_address)
                .size(11)
                .font(iced::Font::MONOSPACE)
                .color(colors::text_secondary(theme))
                .wrapping(text::Wrapping::None)
                .width(Length::Shrink),
        )
        .width(Length::Fixed(240.0))
        .height(Length::Fixed(16.0))
        .clip(true);

        let addr_row = row![
            addr_text,
            Space::with_width(4),
            button(copy_icon)
                .on_press(ChatListMessage::CopyOwnAddress)
                .padding(4)
                .style(move |t: &Theme, status| styles::button_icon(t, status)),
        ]
        .align_y(Alignment::Center)
        .spacing(0);

        let header = container(column![name_row, addr_row].spacing(4))
            .width(Length::Fill)
            .padding(Padding::from([12, 12]))
            .style(move |t: &Theme| styles::panel_header(t));

        let body_col = column![
            self.build_chats_section(theme),
            self.build_incoming_pending(theme),
            self.build_outgoing_pending(theme),
            self.build_contacts_list(theme)
        ]
        .spacing(10);
        let body = scrollable(body_col.padding([0, 4]));
        let panel = column![
            header,
            container(Space::with_height(1))
                .width(Length::Fill)
                .style(move |t: &Theme| styles::divider(t)),
            body
        ]
        .width(Length::Fixed(320.0))
        .spacing(0);
        container(panel)
            .width(Length::Fixed(320.0))
            .height(Length::Fill)
            .style(move |t: &Theme| container::Style {
                background: Some(iced::Background::Color(colors::background_base(t))),
                ..Default::default()
            })
            .into()
    }

    fn build_chats_section(&self, theme: &Theme) -> Element<'_, ChatListMessage> {
        let icon_color = colors::text_primary(theme);
        let add_icon = svg::Svg::new(svg::Handle::from_memory(ADD_ICON.as_bytes().to_vec()))
            .width(Length::Fixed(20.0))
            .height(Length::Fixed(20.0))
            .style(move |_theme, _status| svg::Style {
                color: Some(icon_color),
            });

        let settings_icon =
            svg::Svg::new(svg::Handle::from_memory(SETTINGS_ICON.as_bytes().to_vec()))
                .width(Length::Fixed(20.0))
                .height(Length::Fixed(20.0))
                .style(move |_theme, _status| svg::Style {
                    color: Some(icon_color),
                });

        let chats_header = container(
            row![
                button(add_icon)
                    .on_press(ChatListMessage::ShowAddContactModal)
                    .padding(4)
                    .style(move |t: &Theme, status| styles::button_icon(t, status)),
                Space::with_width(8),
                text("Chats").size(16).color(colors::text_primary(theme)),
                Space::with_width(Length::Fill),
                button(settings_icon)
                    .on_press(ChatListMessage::OpenSettings)
                    .padding(4)
                    .style(move |t: &Theme, status| styles::button_icon(t, status)),
            ]
            .align_y(Alignment::Center),
        )
        .padding(Padding::from([8, 12]));

        column![chats_header].into()
    }

    fn build_incoming_call_overlay<'a>(
        &self,
        incoming: IncomingCallInfo,
        background: Element<'a, ChatListMessage>,
        theme: &Theme,
    ) -> Element<'a, ChatListMessage> {
        let icon_color = colors::text_primary(theme);
        let phone_icon = svg::Svg::new(svg::Handle::from_memory(
            PHONE_CALL_ICON.as_bytes().to_vec(),
        ))
        .width(Length::Fixed(20.0))
        .height(Length::Fixed(20.0))
        .style(move |_theme, _status| svg::Style {
            color: Some(icon_color),
        });

        let left_block = row![
            phone_icon,
            Space::with_width(8),
            column![
                row![
                    text("Incoming Call").size(14),
                    Space::with_width(8),
                    text(incoming.name).size(16),
                ]
                .align_y(Alignment::Center),
                text(incoming.address.clone())
                    .size(11)
                    .color(colors::text_secondary(theme)),
            ]
            .spacing(2)
        ]
        .align_y(Alignment::Center);
        let actions = row![
            button(text("Accept").size(14))
                .on_press(ChatListMessage::AcceptCall(incoming.address.clone()))
                .padding(8)
                .style(button::primary),
            Space::with_width(8),
            button(text("Reject").size(14))
                .on_press(ChatListMessage::RejectCall(incoming.address.clone()))
                .padding(8)
                .style(button::danger),
        ]
        .align_y(Alignment::Center);
        let top_bar = container(
            row![left_block, Space::with_width(Length::Fill), actions].align_y(Alignment::Center),
        )
        .width(Length::Fill)
        .height(Length::Fixed(56.0))
        .padding(Padding::from([8, 12]))
        .style(move |t: &Theme| styles::panel_header(t));
        container(column![top_bar, background])
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    fn build_active_call_overlay<'a>(
        &self,
        call: CallInfo,
        background: Element<'a, ChatListMessage>,
        theme: &Theme,
    ) -> Element<'a, ChatListMessage> {
        let icon_color = colors::text_primary(theme);
        let phone_icon = svg::Svg::new(svg::Handle::from_memory(
            PHONE_CALL_ICON.as_bytes().to_vec(),
        ))
        .width(Length::Fixed(20.0))
        .height(Length::Fixed(20.0))
        .style(move |_theme, _status| svg::Style {
            color: Some(icon_color),
        });

        let (status_text, status_color) = match call.state {
            CallState::Calling => ("Calling...", colors::text_secondary(theme)),

            CallState::Ringing => ("Ringing...", colors::text_secondary(theme)),

            CallState::Connected => ("Connected", colors::text_success(theme)),
        };

        let mic_icon = svg::Svg::new(svg::Handle::from_memory(if self.is_muted {
            MIC_OFF_ICON.as_bytes().to_vec()
        } else {
            MIC_ON_ICON.as_bytes().to_vec()
        }))
        .width(Length::Fixed(16.0))
        .height(Length::Fixed(16.0))
        .style(move |_theme, _status| svg::Style {
            color: Some(icon_color),
        });

        let left_block = row![
            phone_icon,
            Space::with_width(8),
            column![
                row![
                    text(status_text).size(14).color(status_color),
                    Space::with_width(8),
                    mic_icon,
                    Space::with_width(4),
                    text(call.name).size(16),
                ]
                .align_y(Alignment::Center),
                text(call.address.clone())
                    .size(11)
                    .color(colors::text_secondary(theme)),
            ]
            .spacing(2)
        ]
        .align_y(Alignment::Center);

        let end_call_btn = button(text("End Call").size(14))
            .on_press(ChatListMessage::HangupCall(call.address.clone()))
            .padding(8)
            .style(button::danger);

        let audio_settings_btn = button(
            svg::Svg::new(svg::Handle::from_memory(SETTINGS_ICON.as_bytes().to_vec()))
                .width(Length::Fixed(18.0))
                .height(Length::Fixed(18.0)),
        )
        .on_press(if self.show_audio_settings {
            ChatListMessage::HideAudioSettings
        } else {
            ChatListMessage::ShowAudioSettings
        })
        .padding(8)
        .style(if self.show_audio_settings {
            button::primary
        } else {
            button::secondary
        });

        let right_controls = row![
            button(
                svg::Svg::new(svg::Handle::from_memory(if self.is_muted {
                    MIC_OFF_ICON.as_bytes().to_vec()
                } else {
                    MIC_ON_ICON.as_bytes().to_vec()
                }))
                .width(Length::Fixed(20.0))
                .height(Length::Fixed(20.0))
                .style(move |_theme, _status| svg::Style {
                    color: Some(icon_color),
                }),
            )
            .on_press(ChatListMessage::ToggleMute)
            .padding(8)
            .style(if self.is_muted {
                button::danger
            } else {
                button::secondary
            }),
            Space::with_width(8),
            audio_settings_btn,
            Space::with_width(8),
            end_call_btn
        ]
        .align_y(Alignment::Center);

        let top_bar = container(
            row![left_block, Space::with_width(Length::Fill), right_controls]
                .align_y(Alignment::Center),
        )
        .width(Length::Fill)
        .height(Length::Fixed(56.0))
        .padding(Padding::from([8, 12]))
        .style(move |t: &Theme| styles::panel_header(t));

        let base_content = column![top_bar, background];

        if self.show_audio_settings {
            // Create audio settings panel
            let input_section = column![
                row![
                    svg::Svg::new(svg::Handle::from_memory(
                        MIC_SETTINGS_ICON.as_bytes().to_vec(),
                    ))
                    .width(Length::Fixed(16.0))
                    .height(Length::Fixed(16.0)),
                    Space::with_width(8),
                    text("Microphone").size(14)
                ]
                .align_y(Alignment::Center),
                Space::with_height(8),
                scrollable(
                    column(
                        self.available_input_devices
                            .iter()
                            .map(|device| {
                                let is_selected = self
                                    .selected_input_device
                                    .as_ref()
                                    .map(|d| d == device)
                                    .unwrap_or(false);
                                button(
                                    container(text(device.clone()).size(12))
                                        .width(Length::Fill)
                                        .padding([4, 8]),
                                )
                                .on_press(ChatListMessage::SelectInputDevice(device.clone()))
                                .width(Length::Fill)
                                .style(if is_selected {
                                    button::primary
                                } else {
                                    button::secondary
                                })
                                .into()
                            })
                            .collect::<Vec<Element<'_, ChatListMessage>>>()
                    )
                    .spacing(4)
                )
                .height(Length::Fixed(100.0)),
                Space::with_height(8),
                row![
                    svg::Svg::new(svg::Handle::from_memory(
                        MIC_SETTINGS_ICON.as_bytes().to_vec(),
                    ))
                    .width(Length::Fixed(14.0))
                    .height(Length::Fixed(14.0)),
                    Space::with_width(6),
                    text("Volume").size(12)
                ]
                .align_y(Alignment::Center),
                Space::with_height(4),
                row![
                    slider(
                        0.0..=2.0,
                        self.microphone_volume,
                        ChatListMessage::MicrophoneVolumeChanged
                    )
                    .step(0.1),
                    Space::with_width(8),
                    text(format!("{}%", (self.microphone_volume * 100.0) as i32))
                        .size(12)
                        .width(Length::Fixed(48.0))
                ]
                .align_y(Alignment::Center)
            ]
            .spacing(4);

            let output_section = column![
                row![
                    svg::Svg::new(svg::Handle::from_memory(SPEAKER_ICON.as_bytes().to_vec(),))
                        .width(Length::Fixed(16.0))
                        .height(Length::Fixed(16.0)),
                    Space::with_width(8),
                    text("Speaker").size(14)
                ]
                .align_y(Alignment::Center),
                Space::with_height(8),
                scrollable(
                    column(
                        self.available_output_devices
                            .iter()
                            .map(|device| {
                                let is_selected = self
                                    .selected_output_device
                                    .as_ref()
                                    .map(|d| d == device)
                                    .unwrap_or(false);
                                button(
                                    container(text(device.clone()).size(12))
                                        .width(Length::Fill)
                                        .padding([4, 8]),
                                )
                                .on_press(ChatListMessage::SelectOutputDevice(device.clone()))
                                .width(Length::Fill)
                                .style(if is_selected {
                                    button::primary
                                } else {
                                    button::secondary
                                })
                                .into()
                            })
                            .collect::<Vec<Element<'_, ChatListMessage>>>()
                    )
                    .spacing(4)
                )
                .height(Length::Fixed(100.0)),
                Space::with_height(8),
                row![
                    svg::Svg::new(svg::Handle::from_memory(SPEAKER_ICON.as_bytes().to_vec(),))
                        .width(Length::Fixed(14.0))
                        .height(Length::Fixed(14.0)),
                    Space::with_width(6),
                    text("Volume").size(12)
                ]
                .align_y(Alignment::Center),
                Space::with_height(4),
                row![
                    slider(
                        0.0..=2.0,
                        self.speaker_volume,
                        ChatListMessage::SpeakerVolumeChanged
                    )
                    .step(0.1),
                    Space::with_width(8),
                    text(format!("{}%", (self.speaker_volume * 100.0) as i32))
                        .size(12)
                        .width(Length::Fixed(48.0))
                ]
                .align_y(Alignment::Center)
            ]
            .spacing(4);

            let settings_panel = container(
                column![input_section, Space::with_height(16), output_section,].spacing(12),
            )
            .width(Length::Fixed(280.0))
            .padding(16)
            .style(move |t: &Theme| styles::card(t));

            // Position settings panel in top-right with proper alignment
            let settings_overlay = container(
                container(settings_panel)
                    .width(Length::Shrink)
                    .height(Length::Shrink),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(iced::alignment::Horizontal::Right)
            .align_y(iced::alignment::Vertical::Top)
            .padding(Padding::ZERO.top(64).right(12));

            // Use stack to layer settings over base content
            stack![base_content, settings_overlay].into()
        } else {
            base_content.into()
        }
    }

    fn build_add_contact_modal(&self, theme: &Theme) -> Element<'_, ChatListMessage> {
        // Create modal content
        let modal_dialog = container(
            column![
                row![
                    text("Add Contact")
                        .size(20)
                        .color(colors::text_primary(theme)),
                    Space::with_width(Length::Fill),
                    button(text("×").size(24))
                        .on_press(ChatListMessage::HideAddContactModal)
                        .padding(4)
                        .style(button::text)
                ]
                .align_y(Alignment::Center),
                Space::with_height(16),
                container(
                    text("Contact Address")
                        .size(14)
                        .color(colors::text_secondary(theme))
                )
                .padding(Padding::ZERO.bottom(4)),
                text_input("Enter contact address", &self.add_contact_addr)
                    .on_input(ChatListMessage::AddContactInputChanged)
                    .on_submit(ChatListMessage::AddContactSubmit)
                    .padding(10)
                    .size(16),
                if let Some(err) = &self.add_contact_error {
                    Element::from(
                        container(text(err).size(12).color(colors::text_error(theme))).padding(4),
                    )
                } else {
                    Element::from(Space::with_height(0))
                },
                Space::with_height(16),
                row![
                    Space::with_width(Length::Fill),
                    button(text("Cancel").size(14))
                        .on_press(ChatListMessage::HideAddContactModal)
                        .padding([8, 16])
                        .style(button::secondary),
                    Space::with_width(8),
                    button(text("Add Contact").size(14))
                        .on_press(ChatListMessage::AddContactSubmit)
                        .padding([8, 16])
                        .style(
                            if self.add_contact_error.is_none()
                                && !self.add_contact_addr.trim().is_empty()
                            {
                                button::primary
                            } else {
                                button::secondary
                            }
                        )
                ]
            ]
            .spacing(8),
        )
        .width(Length::Fixed(420.0))
        .padding(24)
        .style(move |t: &Theme| styles::card(t));

        // Create overlay background that dims the main content and centers the modal
        container(
            container(modal_dialog)
                .center_x(Length::Fill)
                .center_y(Length::Fill)
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |t: &Theme| styles::modal_overlay(t))
        .into()
    }

    fn build_incoming_pending(&self, theme: &Theme) -> Element<'_, ChatListMessage> {
        if self.incoming_pending.is_empty() {
            return Space::with_height(0).into();
        }
        let mut col = column![].spacing(6);
        for p in &self.incoming_pending {
            let accept_btn = button(text("Accept").size(12).color(Color::WHITE))
                .on_press(ChatListMessage::AcceptIncoming(p.address.clone()))
                .padding(Padding::from([4, 8]))
                .style(button::success);

            let reject_btn = button(text("Reject").size(12).color(Color::WHITE))
                .on_press(ChatListMessage::RejectIncoming(p.address.clone()))
                .padding(Padding::from([4, 8]))
                .style(button::danger);

            let row = column![
                row![
                    text(&p.name).size(14),
                    Space::with_width(Length::Fill),
                    accept_btn,
                    Space::with_width(4),
                    reject_btn
                ]
                .align_y(Alignment::Center),
                text(&p.address)
                    .size(11)
                    .font(iced::Font::MONOSPACE)
                    .color(colors::text_secondary(theme))
            ]
            .spacing(2);

            col = col.push(
                container(row)
                    .padding(8)
                    .style(move |t: &Theme| styles::card(t)),
            );
        }
        col.into()
    }

    fn build_outgoing_pending(&self, theme: &Theme) -> Element<'_, ChatListMessage> {
        if self.outgoing_pending.is_empty() {
            return Space::with_height(0).into();
        }
        let mut col = column![
            text("Pending Requests")
                .size(14)
                .color(colors::text_secondary(theme))
        ]
        .spacing(6);
        for p in &self.outgoing_pending {
            let cancel_btn = button(text("Cancel").size(12))
                .on_press(ChatListMessage::CancelOutgoing(p.address.clone()))
                .padding(Padding::from([4, 8]))
                .style(button::secondary);

            let content = column![
                row![
                    text("Outgoing Request").size(14),
                    Space::with_width(Length::Fill),
                    cancel_btn
                ]
                .align_y(Alignment::Center),
                text(&p.address)
                    .size(11)
                    .font(iced::Font::MONOSPACE)
                    .color(colors::text_secondary(theme))
            ]
            .spacing(2);

            col = col.push(
                container(content)
                    .padding(8)
                    .style(move |t: &Theme| styles::card(t)),
            );
        }
        col.into()
    }

    fn build_contacts_list(&self, theme: &Theme) -> Element<'_, ChatListMessage> {
        let mut col = column![].spacing(6);
        for c in &self.contacts {
            let connected = c.connected;
            let success_bg = colors::success_bg(theme);
            let success_border = colors::success_border(theme);
            let divider_color = colors::divider(theme);
            let status_circle = container(Space::new(8, 8)).style(move |_t: &Theme| {
                if connected {
                    container::Style {
                        background: Some(iced::Background::Color(success_bg)),
                        border: iced::Border {
                            color: success_border,
                            width: 1.0,
                            radius: 4.0.into(),
                        },
                        ..Default::default()
                    }
                } else {
                    container::Style {
                        background: Some(iced::Background::Color(Color::TRANSPARENT)),
                        border: iced::Border {
                            color: divider_color,
                            width: 1.0,
                            radius: 4.0.into(),
                        },
                        ..Default::default()
                    }
                }
            });

            let display_name = if c.name.is_empty() {
                &c.address
            } else {
                &c.name
            };

            let mut content = column![
                row![
                    text(display_name).size(14),
                    Space::with_width(Length::Fill),
                    status_circle
                ]
                .align_y(Alignment::Center)
            ]
            .spacing(2);

            if let Some(last_msg) = &c.last_message {
                let truncated = if last_msg.chars().count() > 30 {
                    format!("{}...", &last_msg.chars().take(30).collect::<String>())
                } else {
                    last_msg.clone()
                };
                content = content.push(text(truncated).size(11).color(colors::text_muted(theme)));
            }

            let is_selected = self.selected_chat.as_ref() == Some(&c.address);
            let addr = c.address.clone();

            let button_style = if is_selected {
                button::primary
            } else {
                button::secondary
            };

            col = col.push(
                button(content)
                    .on_press(ChatListMessage::SelectChat(addr))
                    .padding(8)
                    .width(Length::Fill)
                    .style(button_style),
            );
        }
        col.into()
    }

    fn build_right_panel<'a>(&'a self, theme: &'a Theme) -> Element<'a, ChatListMessage> {
        if self.selected_chat.is_none() {
            return container(
                text("Select a chat to start messaging")
                    .size(18)
                    .color(colors::text_secondary(theme)),
            )
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into();
        }

        let header = self.build_chat_header(theme);
        let body = self.build_chat_body(theme);
        let footer = self.build_chat_footer(theme);
        let mut col = column![
            header,
            container(Space::with_height(1))
                .width(Length::Fill)
                .style(styles::divider),
            body,
            footer
        ]
        .spacing(0)
        .width(Length::Fill)
        .height(Length::Fill);

        if let Some(err) = &self.global_error {
            col = col.push(
                container(text(err).color(colors::text_error(theme)))
                    .padding(8)
                    .width(Length::Fill),
            );
        }
        container(col)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    fn build_chat_header<'a>(&'a self, theme: &'a Theme) -> Element<'a, ChatListMessage> {
        let (name, address, connected) = match self.selected_chat.as_ref() {
            Some(addr) => {
                if let Some(c) = self.contacts.iter().find(|c| &c.address == addr) {
                    (c.name.clone(), c.address.clone(), c.connected)
                } else {
                    ("".to_string(), addr.clone(), false)
                }
            }
            None => return Space::with_height(0).into(),
        };

        let display_name = if name.is_empty() {
            address.clone()
        } else {
            name
        };

        let is_connected = connected;
        let success_bg = colors::success_bg(theme);
        let success_border = colors::success_border(theme);
        let divider_color = colors::divider(theme);
        let status_circle = container(Space::new(10, 10)).style(move |_t: &Theme| {
            if is_connected {
                container::Style {
                    background: Some(iced::Background::Color(success_bg)),
                    border: iced::Border {
                        color: success_border,
                        width: 1.5,
                        radius: 5.0.into(),
                    },
                    ..Default::default()
                }
            } else {
                container::Style {
                    background: Some(iced::Background::Color(Color::TRANSPARENT)),
                    border: iced::Border {
                        color: divider_color,
                        width: 1.5,
                        radius: 5.0.into(),
                    },
                    ..Default::default()
                }
            }
        });

        let status_text = if connected {
            "connected"
        } else {
            "disconnected"
        };

        let icon_color = colors::text_primary(theme);
        let phone_icon = svg::Svg::new(svg::Handle::from_memory(
            PHONE_CALL_ICON.as_bytes().to_vec(),
        ))
        .width(Length::Fixed(24.0))
        .height(Length::Fixed(24.0))
        .style(move |_theme, _status| svg::Style {
            color: Some(icon_color),
        });

        let mut title_row_items = vec![
            text(display_name).size(18).into(),
            Space::with_width(Length::Fill).into(),
        ];

        // Add call button only if connected
        if connected {
            title_row_items.push(
                button(phone_icon)
                    .on_press(ChatListMessage::StartVoiceCall(address.clone()))
                    .padding(6)
                    .style(button::text)
                    .into(),
            );
            title_row_items.push(Space::with_width(12).into());
        }

        title_row_items.push(status_circle.into());
        title_row_items.push(Space::with_width(6).into());
        title_row_items.push(
            text(status_text)
                .size(12)
                .color(colors::text_secondary(theme))
                .into(),
        );

        let title_row = row(title_row_items).align_y(Alignment::Center);

        let icon_color = colors::text_primary(theme);
        let copy_icon = svg::Svg::new(svg::Handle::from_memory(COPY_ICON.as_bytes().to_vec()))
            .width(Length::Fixed(16.0))
            .height(Length::Fixed(16.0))
            .style(move |_theme, _status| svg::Style {
                color: Some(icon_color),
            });

        let addr_text = container(
            text(address.clone())
                .size(11)
                .font(iced::Font::MONOSPACE)
                .color(colors::text_secondary(theme))
                .wrapping(text::Wrapping::None)
                .width(Length::Shrink),
        )
        .max_width(520)
        .height(Length::Fixed(16.0))
        .clip(true);

        let addr_row = row![
            addr_text,
            Space::with_width(4),
            button(copy_icon)
                .on_press(ChatListMessage::CopyPeerAddress(address))
                .padding(4)
                .style(button::text),
        ]
        .align_y(Alignment::Center)
        .spacing(0);

        let header_content = column![title_row, addr_row].spacing(4);

        container(header_content)
            .width(Length::Fill)
            .padding(Padding::from([12, 16]))
            .style(move |t: &Theme| styles::panel_header(t))
            .into()
    }

    fn build_chat_body(&self, theme: &Theme) -> Element<'_, ChatListMessage> {
        let msgs = match self.selected_chat.as_ref() {
            Some(addr) => self.messages_by_addr.get(addr).cloned().unwrap_or_default(),
            None => Vec::new(),
        };

        let mut col = column![].spacing(10);

        for msg in msgs {
            let is_mine = msg.is_mine;
            let delivered = msg.delivered;
            let bubble_content = column![
                text(msg.text).size(14).color(colors::text_primary(theme)),
                text(msg.timestamp)
                    .size(10)
                    .color(colors::text_muted(theme))
            ]
            .spacing(4);

            let bubble =
                container(bubble_content)
                    .padding(10)
                    .max_width(400)
                    .style(move |t: &Theme| {
                        let (bg_color, border_color) = if is_mine {
                            if delivered {
                                // Delivered outgoing
                                (
                                    colors::message_outgoing_bg(t),
                                    colors::message_outgoing_border(t),
                                )
                            } else {
                                // Pending outgoing
                                (
                                    colors::message_pending_bg(t),
                                    colors::message_pending_border(t),
                                )
                            }
                        } else {
                            // Incoming
                            (
                                colors::message_incoming_bg(t),
                                colors::message_incoming_border(t),
                            )
                        };

                        container::Style {
                            background: Some(iced::Background::Color(bg_color)),
                            border: iced::Border {
                                color: border_color,
                                width: 1.0,
                                radius: 8.0.into(),
                            },
                            ..Default::default()
                        }
                    });

            let row_line = if msg.is_mine {
                row![
                    Space::with_width(Length::Fill),
                    bubble,
                    Space::with_width(12)
                ]
            } else {
                row![
                    Space::with_width(12),
                    bubble,
                    Space::with_width(Length::Fill)
                ]
            };

            col = col.push(row_line);
        }

        let sc = scrollable(col.padding(16))
            .height(Length::Fill)
            .id(self.messages_scrollable_id.clone());

        container(sc)
            .height(Length::Fill)
            .style(move |t: &Theme| container::Style {
                background: Some(iced::Background::Color(colors::background_base(t))),
                ..Default::default()
            })
            .into()
    }

    fn build_chat_footer(&self, theme: &Theme) -> Element<'_, ChatListMessage> {
        let can_send = self.selected_chat.is_some() && !self.compose_text.trim().is_empty();

        let icon_color = colors::text_primary(theme);
        let send_icon = svg::Svg::new(svg::Handle::from_memory(SEND_ICON.as_bytes().to_vec()))
            .width(Length::Fixed(20.0))
            .height(Length::Fixed(20.0))
            .style(move |_theme, _status| svg::Style {
                color: Some(icon_color),
            });

        let mut send_btn = button(send_icon).padding(8);
        if can_send {
            send_btn = send_btn
                .on_press(ChatListMessage::SendMessage)
                .style(button::primary);
        } else {
            send_btn = send_btn.style(button::secondary);
        }

        let input = text_input("Type a message...", &self.compose_text)
            .on_input(ChatListMessage::ComposeChanged)
            .padding(10)
            .size(14)
            .width(Length::Fill)
            .on_submit(ChatListMessage::SendMessage);

        container(
            row![input, Space::with_width(8), send_btn]
                .align_y(Alignment::Center)
                .padding(12),
        )
        .width(Length::Fill)
        .style(move |t: &Theme| container::Style {
            background: Some(iced::Background::Color(colors::background_weak(t))),
            ..Default::default()
        })
        .into()
    }

    fn validate_address(s: &str) -> Option<String> {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return Some("Address cannot be empty".into());
        }
        if !trimmed.chars().any(|c| c.is_alphanumeric()) {
            return Some("Invalid address".into());
        }
        None
    }
}

// Screen trait implementation for integration with the new architecture
impl Screen for ChatListScreen {
    type Message = ChatListMessage;

    fn update(
        &mut self,
        message: ChatListMessage,
        ctx: &mut AppContext,
    ) -> ScreenCommand<ChatListMessage> {
        // Handle messages that need special processing
        match message {
            ChatListMessage::AddContactInputChanged(ref value) => {
                ctx.pending_add_addr = Some(value.clone());
                // Call the internal update method for other messages
                let cmd = self.update_internal(message);
                return ScreenCommand::Message(cmd);
            }
            ChatListMessage::ComposeChanged(ref value) => {
                ctx.pending_compose_text = Some(value.clone());
                // Call the internal update method for other messages
                let cmd = self.update_internal(message);
                return ScreenCommand::Message(cmd);
            }
            ChatListMessage::AddContactSubmit => {
                // Handle add contact with async operation
                let addr_str = ctx.pending_add_addr.clone().unwrap_or_default();
                if !addr_str.is_empty() {
                    let cm = ctx.contact_manager.clone();
                    let ui_tx = ctx.ui_event_tx.clone();
                    let add_contact_cmd = Task::perform(
                        async move {
                            if let Ok(address) = addr_str.parse::<ntied_transport::Address>() {
                                if let Some(cm) = cm {
                                    let _ = cm.connect_contact(address).await;
                                }
                                let _ = ui_tx
                                    .send(crate::ui::UiEvent::OutgoingRequest {
                                        address: addr_str.clone(),
                                    })
                                    .await;
                            }
                            ChatListMessage::Noop
                        },
                        |msg| msg,
                    );
                    let ui_cmd = self.update_internal(ChatListMessage::AddContactSubmit);
                    return ScreenCommand::Message(Task::batch(vec![ui_cmd, add_contact_cmd]));
                } else {
                    // If address is empty, just update UI
                    let ui_cmd = self.update_internal(ChatListMessage::AddContactSubmit);
                    return ScreenCommand::Message(ui_cmd);
                }
            }
            ChatListMessage::AcceptIncoming(ref addr_str) => {
                // Handle accept incoming with async operation
                let cm = ctx.contact_manager.clone();
                let chats = ctx.chat_manager.clone();
                let ui_tx = ctx.ui_event_tx.clone();
                let addr_str_async = addr_str.clone();
                let accept_cmd = Task::perform(
                    async move {
                        if let (Some(cm), Some(chats)) = (cm, chats) {
                            if let Ok(address) = addr_str_async.parse::<ntied_transport::Address>()
                            {
                                let handle = cm.connect_contact(address).await;
                                let _ = handle.accept().await;
                                let name = handle
                                    .profile()
                                    .map(|p| p.name)
                                    .unwrap_or_else(|| address.to_string());
                                if let Some(pk) = handle.public_key() {
                                    let _ = chats
                                        .add_contact_chat(address, pk, name.clone(), None)
                                        .await;
                                }
                                let _ = ui_tx
                                    .send(crate::ui::UiEvent::ContactAccepted {
                                        address: addr_str_async.clone(),
                                        name,
                                    })
                                    .await;
                            }
                        }
                        ChatListMessage::Noop
                    },
                    |msg| msg,
                );
                let ui_cmd =
                    self.update_internal(ChatListMessage::AcceptIncoming(addr_str.clone()));
                return ScreenCommand::Message(Task::batch(vec![ui_cmd, accept_cmd]));
            }
            ChatListMessage::RejectIncoming(ref addr_str) => {
                // Handle reject incoming with async operation
                let cm = ctx.contact_manager.clone();
                let ui_tx = ctx.ui_event_tx.clone();
                let addr_str_async = addr_str.clone();

                let reject_cmd = Task::perform(
                    async move {
                        if let Some(cm) = cm {
                            if let Ok(address) = addr_str_async.parse::<ntied_transport::Address>()
                            {
                                let handle = cm.connect_contact(address).await;
                                let _ = handle.reject().await;
                                let _ = ui_tx
                                    .send(crate::ui::UiEvent::ContactRemoved {
                                        address: addr_str_async.clone(),
                                    })
                                    .await;
                            }
                        }
                        ChatListMessage::Noop
                    },
                    |msg| msg,
                );

                let ui_cmd =
                    self.update_internal(ChatListMessage::RejectIncoming(addr_str.clone()));
                return ScreenCommand::Message(Task::batch(vec![ui_cmd, reject_cmd]));
            }
            ChatListMessage::CancelOutgoing(ref addr_str) => {
                // Handle cancel outgoing with async operation
                let cm = ctx.contact_manager.clone();
                let ui_tx = ctx.ui_event_tx.clone();
                let addr_str_async = addr_str.clone();

                let cancel_cmd = Task::perform(
                    async move {
                        if let Some(cm) = cm {
                            if let Ok(address) = addr_str_async.parse::<ntied_transport::Address>()
                            {
                                let handle = cm.connect_contact(address).await;
                                let _ = handle.reject().await;
                                let _ = ui_tx
                                    .send(crate::ui::UiEvent::ContactRemoved {
                                        address: addr_str_async.clone(),
                                    })
                                    .await;
                            }
                        }
                        ChatListMessage::Noop
                    },
                    |msg| msg,
                );

                let ui_cmd =
                    self.update_internal(ChatListMessage::CancelOutgoing(addr_str.clone()));
                return ScreenCommand::Message(Task::batch(vec![ui_cmd, cancel_cmd]));
            }
            ChatListMessage::StartVoiceCall(ref address) => {
                // Handle voice call with async operation
                let call_mgr = ctx.call_manager.clone();
                let address = address.clone();

                let call_cmd = Task::perform(
                    async move {
                        if let Some(mgr) = call_mgr {
                            if let Ok(addr) = address.parse::<ntied_transport::Address>() {
                                let _ = mgr.start_call(addr).await;
                            }
                        }
                        ChatListMessage::Noop
                    },
                    |msg| msg,
                );

                return ScreenCommand::Message(call_cmd);
            }
            ChatListMessage::AcceptCall(ref address) => {
                // Handle accept call with async operation
                let call_mgr = ctx.call_manager.clone();
                let address = address.clone();

                let call_cmd = Task::perform(
                    async move {
                        if let Some(mgr) = call_mgr {
                            if let Ok(addr) = address.parse::<ntied_transport::Address>() {
                                let _ = mgr.accept_call(addr).await;
                            }
                        }
                        ChatListMessage::Noop
                    },
                    |msg| msg,
                );

                return ScreenCommand::Message(call_cmd);
            }
            ChatListMessage::RejectCall(ref address) => {
                // Handle reject call with async operation
                let call_mgr = ctx.call_manager.clone();
                let address = address.clone();

                let call_cmd = Task::perform(
                    async move {
                        if let Some(mgr) = call_mgr {
                            if let Ok(addr) = address.parse::<ntied_transport::Address>() {
                                let _ = mgr.reject_call(addr).await;
                            }
                        }
                        ChatListMessage::Noop
                    },
                    |msg| msg,
                );

                return ScreenCommand::Message(call_cmd);
            }
            ChatListMessage::HangupCall(ref address) => {
                // Handle hangup call with async operation
                let call_mgr = ctx.call_manager.clone();
                let address = address.clone();

                let call_cmd = Task::perform(
                    async move {
                        if let Some(mgr) = call_mgr {
                            if let Ok(addr) = address.parse::<ntied_transport::Address>() {
                                let _ = mgr.end_call(addr).await;
                            }
                        }
                        ChatListMessage::Noop
                    },
                    |msg| msg,
                );

                return ScreenCommand::Message(call_cmd);
            }
            ChatListMessage::ToggleMute => {
                // Handle mute toggle with async operation and return actual state
                let call_mgr = ctx.call_manager.clone();

                let mute_cmd = Task::perform(
                    async move {
                        if let Some(mgr) = call_mgr {
                            // toggle_mute returns the new mute state
                            if let Ok(is_muted) = mgr.toggle_mute().await {
                                return ChatListMessage::MuteToggled(is_muted);
                            }
                        }
                        ChatListMessage::Noop
                    },
                    |msg| msg,
                );

                return ScreenCommand::Message(mute_cmd);
            }
            ChatListMessage::SelectInputDevice(ref device_name) => {
                // Handle input device switch with async operation
                let call_mgr = ctx.call_manager.clone();
                let device = device_name.clone();

                let switch_cmd = Task::perform(
                    async move {
                        if let Some(mgr) = call_mgr {
                            let _ = mgr.switch_input_device(Some(device)).await;
                        }
                        ChatListMessage::Noop
                    },
                    |msg| msg,
                );

                // Also update UI state
                let ui_cmd =
                    self.update_internal(ChatListMessage::SelectInputDevice(device_name.clone()));
                return ScreenCommand::Message(Task::batch(vec![ui_cmd, switch_cmd]));
            }
            ChatListMessage::SelectOutputDevice(ref device_name) => {
                // Handle output device switch with async operation
                let call_mgr = ctx.call_manager.clone();
                let device = device_name.clone();

                let switch_cmd = Task::perform(
                    async move {
                        if let Some(mgr) = call_mgr {
                            let _ = mgr.switch_output_device(Some(device)).await;
                        }
                        ChatListMessage::Noop
                    },
                    |msg| msg,
                );

                // Also update UI state
                let ui_cmd =
                    self.update_internal(ChatListMessage::SelectOutputDevice(device_name.clone()));
                return ScreenCommand::Message(Task::batch(vec![ui_cmd, switch_cmd]));
            }
            ChatListMessage::SpeakerVolumeChanged(volume) => {
                // Handle speaker volume change with async operation
                let call_mgr = ctx.call_manager.clone();

                let volume_cmd = Task::perform(
                    async move {
                        if let Some(mgr) = call_mgr {
                            let _ = mgr.set_playback_volume(volume).await;
                        }
                        ChatListMessage::Noop
                    },
                    |msg| msg,
                );

                // Also update UI state
                let ui_cmd = self.update_internal(ChatListMessage::SpeakerVolumeChanged(volume));
                return ScreenCommand::Message(Task::batch(vec![ui_cmd, volume_cmd]));
            }
            ChatListMessage::MicrophoneVolumeChanged(volume) => {
                // Handle microphone volume change with async operation
                let call_mgr = ctx.call_manager.clone();

                let volume_cmd = Task::perform(
                    async move {
                        if let Some(mgr) = call_mgr {
                            let _ = mgr.set_capture_volume(volume).await;
                        }
                        ChatListMessage::Noop
                    },
                    |msg| msg,
                );

                // Also update UI state
                let ui_cmd = self.update_internal(ChatListMessage::MicrophoneVolumeChanged(volume));
                return ScreenCommand::Message(Task::batch(vec![ui_cmd, volume_cmd]));
            }
            ChatListMessage::SelectChat(ref addr) => {
                ctx.selected_chat_addr = Some(addr.clone());

                // Load chat history
                let chats = ctx.chat_manager.clone();
                let ui_tx = ctx.ui_event_tx.clone();
                let addr_str = addr.clone();

                let load_history = Task::perform(
                    async move {
                        if let Some(chats) = chats {
                            if let Ok(address) = addr_str.parse::<ntied_transport::Address>() {
                                if let Some(handle) = chats.get_contact_chat(address).await {
                                    let limit = 200usize;
                                    if let Ok(messages) = handle.load_history(limit).await {
                                        for m in messages {
                                            let text = match m.kind {
                                                crate::models::MessageKind::Text(s) => s,
                                            };
                                            if m.log_id.is_some() {
                                                if m.incoming {
                                                    let _ = ui_tx
                                                        .send(crate::ui::UiEvent::NewMessage {
                                                            id: m.id,
                                                            address: addr_str.clone(),
                                                            incoming: true,
                                                            text,
                                                        })
                                                        .await;
                                                } else {
                                                    // Outgoing confirmed: ensure delivered bubble
                                                    let _ = ui_tx
                                                        .send(crate::ui::UiEvent::NewMessage {
                                                            id: m.id,
                                                            address: addr_str.clone(),
                                                            incoming: false,
                                                            text,
                                                        })
                                                        .await;
                                                }
                                            } else {
                                                if m.incoming {
                                                    // Unexpected but safe: treat as delivered incoming
                                                    let _ = ui_tx
                                                        .send(crate::ui::UiEvent::NewMessage {
                                                            id: m.id,
                                                            address: addr_str.clone(),
                                                            incoming: true,
                                                            text,
                                                        })
                                                        .await;
                                                } else {
                                                    // Our pending outgoing (undelivered)
                                                    let _ = ui_tx
                                                        .send(crate::ui::UiEvent::MessageSent {
                                                            id: m.id,
                                                            address: addr_str.clone(),
                                                            text,
                                                        })
                                                        .await;
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        ChatListMessage::Noop
                    },
                    |msg| msg,
                );

                // Call the internal update method and combine with history loading
                let scroll_cmd = self.update_internal(ChatListMessage::SelectChat(addr.clone()));
                return ScreenCommand::Message(Task::batch(vec![scroll_cmd, load_history]));
            }
            ChatListMessage::SendMessage => {
                // Handle message sending with async operation
                let chats = ctx.chat_manager.clone();
                let maybe_addr = ctx.selected_chat_addr.clone();
                let maybe_text = ctx.pending_compose_text.clone().or_else(|| {
                    if !self.compose_text.is_empty() {
                        Some(self.compose_text.clone())
                    } else {
                        None
                    }
                });
                let ui_tx = ctx.ui_event_tx.clone();

                if let (Some(addr_str), Some(text)) = (maybe_addr, maybe_text) {
                    let trimmed = text.trim().to_string();
                    if !trimmed.is_empty() {
                        let send_cmd = Task::perform(
                            async move {
                                if let Some(chats) = chats {
                                    if let Ok(address) =
                                        addr_str.parse::<ntied_transport::Address>()
                                    {
                                        if let Some(handle) = chats.get_contact_chat(address).await
                                        {
                                            if let Ok(message) = handle
                                                .send_message(crate::models::MessageKind::Text(
                                                    trimmed.clone(),
                                                ))
                                                .await
                                            {
                                                let _ = ui_tx
                                                    .send(crate::ui::UiEvent::MessageSent {
                                                        id: message.id,
                                                        address: addr_str.clone(),
                                                        text: trimmed,
                                                    })
                                                    .await;
                                            }
                                        }
                                    }
                                }
                                ChatListMessage::Noop
                            },
                            |msg| msg,
                        );

                        // Clear compose text and update UI
                        ctx.pending_compose_text = None;
                        let ui_cmd = self.update_internal(ChatListMessage::SendMessage);
                        return ScreenCommand::Message(Task::batch(vec![ui_cmd, send_cmd]));
                    }
                }

                // If no message to send, just update UI
                let cmd = self.update_internal(ChatListMessage::SendMessage);
                return ScreenCommand::Message(cmd);
            }
            ChatListMessage::OpenSettings => {
                let server_addr = ctx
                    .server_addr
                    .map(|addr| addr.to_string())
                    .unwrap_or_else(|| crate::DEFAULT_SERVER.to_string());
                return ScreenCommand::ChangeScreen(ScreenType::Settings { server_addr });
            }
            ChatListMessage::Logout => {
                ctx.contact_manager = None;
                ctx.chat_manager = None;
                ctx.storage = None;
                return ScreenCommand::ChangeScreen(ScreenType::Unlock);
            }
            _ => {
                // Call the internal update method for other messages
                let cmd = self.update_internal(message);
                ScreenCommand::Message(cmd)
            }
        }
    }

    fn handle_ui_event(
        &mut self,
        event: UiEvent,
        _ctx: &mut AppContext,
    ) -> ScreenCommand<ChatListMessage> {
        // Apply event to internal state
        self.apply_event(event);
        ScreenCommand::None
    }

    fn view<'a>(&'a self, theme: &'a Theme) -> Element<'a, ChatListMessage> {
        self.view(theme)
    }
}

// Helper function removed - no longer needed as we use inline styling
