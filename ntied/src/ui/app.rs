use std::any::TypeId;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;
use iced::futures::sink::SinkExt as _;
use iced::keyboard::{self, key::Named};
use iced::{Element, Subscription, Task, Theme, stream};
use tokio::sync::{Mutex as TokioMutex, mpsc};

use crate::DEFAULT_SERVER;
use crate::audio::RingtonePlayer;
use crate::call::CallManager;
use crate::chat::ChatManager;
use crate::contact::ContactManager;
use crate::packet::ContactProfile;
use crate::storage::Storage;
use crate::ui::UiEvent;
use crate::ui::core::{Screen, ScreenCommand};
use crate::ui::screens::{
    ChatListMessage, ChatListScreen, InitScreen, SettingsScreen, UnlockScreen,
};
use crate::ui::theme::ThemePreference;

use super::core::ScreenType;

enum CurrentScreen {
    Unlock(UnlockScreen),
    Init(InitScreen),
    Chats(ChatListScreen),
    Settings(SettingsScreen),
}

pub struct AppContext {
    pub storage_dir: PathBuf,
    pub storage: Option<Arc<TokioMutex<Storage>>>,
    pub contact_manager: Option<Arc<ContactManager>>,
    pub chat_manager: Option<Arc<ChatManager>>,
    pub call_manager: Option<Arc<CallManager>>,
    pub profile: Option<ContactProfile>,
    pub server_addr: Option<SocketAddr>,
    pub ui_event_tx: mpsc::Sender<UiEvent>,
    pub ui_event_rx: Arc<TokioMutex<mpsc::Receiver<UiEvent>>>,
    pub pending_add_addr: Option<String>,
    pub selected_chat_addr: Option<String>,
    pub pending_compose_text: Option<String>,
    pub ringtone_player: Arc<TokioMutex<RingtonePlayer>>,
    pub theme: ThemePreference,
    // Call state preservation
    pub active_call_address: Option<String>,
    pub active_call_name: Option<String>,
    pub active_call_state: Option<String>, // "calling", "ringing", or "connected"
    pub incoming_call_address: Option<String>,
    pub incoming_call_name: Option<String>,
}

impl AppContext {
    fn new() -> Self {
        let storage_dir = Self::get_data_dir().unwrap();
        let (ui_event_tx, ui_event_rx) = mpsc::channel(100);
        Self {
            storage_dir,
            storage: None,
            contact_manager: None,
            chat_manager: None,
            call_manager: None,
            profile: None,
            server_addr: None,
            ui_event_tx,
            ui_event_rx: Arc::new(TokioMutex::new(ui_event_rx)),
            pending_add_addr: None,
            selected_chat_addr: None,
            pending_compose_text: None,
            ringtone_player: Arc::new(TokioMutex::new(RingtonePlayer::new())),
            theme: ThemePreference::default(),
            active_call_address: None,
            active_call_name: None,
            active_call_state: None,
            incoming_call_address: None,
            incoming_call_name: None,
        }
    }

    fn get_data_dir() -> Result<PathBuf, anyhow::Error> {
        // Check for custom profile directory from environment variable
        if let Ok(custom_dir) = std::env::var("NTIED_PROFILE_DIR") {
            let path = PathBuf::from(custom_dir);
            if path.is_absolute() {
                return Ok(path);
            } else {
                tracing::warn!("NTIED_PROFILE_DIR is not an absolute path, using default");
            }
        }
        // Fall back to default directory
        let base_dir = dirs::config_dir()
            .or_else(|| dirs::data_dir())
            .context("Failed to determine config directory")?;
        Ok(base_dir.join("ntied"))
    }

    fn is_initialized(&self) -> bool {
        let meta = self.storage_dir.join("meta.json");
        let db = self.storage_dir.join("data.db");
        meta.exists() && db.exists()
    }
}

pub struct ChatApp {
    screen: CurrentScreen,
    ctx: AppContext,
    theme: Theme,
}

impl ChatApp {
    /// Helper method to handle ScreenCommand and convert to Command<AppMessage>
    fn handle_screen_command<M, F>(
        &mut self,
        cmd: crate::ui::core::ScreenCommand<M>,
        wrap: F,
    ) -> Task<AppMessage>
    where
        M: Send + 'static,
        F: Fn(M) -> AppMessage + 'static + Send + Sync + Clone,
    {
        match cmd {
            ScreenCommand::None => Task::none(),
            ScreenCommand::Message(task) => task.map(wrap),
            ScreenCommand::ChangeScreen(screen_type) => self.switch_screen(screen_type),
        }
    }

    fn switch_screen(&mut self, screen_type: crate::ui::core::ScreenType) -> Task<AppMessage> {
        let sync_task = match screen_type {
            ScreenType::Chats { .. } => {
                // If there's an active call, sync state with CallManager
                if self.ctx.active_call_address.is_some() {
                    let call_mgr = self.ctx.call_manager.clone();
                    Some(Task::perform(
                        async move {
                            if let Some(mgr) = call_mgr {
                                let is_muted = mgr.is_muted().await.unwrap_or(false);
                                let capture_vol = mgr.get_capture_volume().await.unwrap_or(1.0);
                                let playback_vol = mgr.get_playback_volume().await.unwrap_or(1.0);
                                let input_dev = mgr.get_current_input_device().await;
                                let output_dev = mgr.get_current_output_device().await;

                                AppMessage::ChatList(ChatListMessage::CallStateSynced {
                                    is_muted,
                                    capture_volume: capture_vol,
                                    playback_volume: playback_vol,
                                    input_device: input_dev,
                                    output_device: output_dev,
                                })
                            } else {
                                AppMessage::Tick
                            }
                        },
                        |msg| msg,
                    ))
                } else {
                    None
                }
            }
            _ => None,
        };

        self.screen = match screen_type {
            ScreenType::Unlock => CurrentScreen::Unlock(UnlockScreen::new()),
            ScreenType::Init => CurrentScreen::Init(InitScreen::new()),
            ScreenType::Chats {
                own_name,
                own_address,
            } => {
                let mut screen = ChatListScreen::new(Some(own_name.clone()));
                screen.set_identity(own_name, own_address);

                // Restore call state if exists
                screen.restore_call_state(
                    self.ctx.active_call_address.clone(),
                    self.ctx.active_call_name.clone(),
                    self.ctx.active_call_state.clone(),
                    self.ctx.incoming_call_address.clone(),
                    self.ctx.incoming_call_name.clone(),
                );

                // Initialize contacts list and connection status when creating the screen
                let ui_tx = self.ctx.ui_event_tx.clone();
                let cm_for_list = self.ctx.chat_manager.clone();
                let contact_mgr = self.ctx.contact_manager.clone();
                tokio::spawn(async move {
                    // Send transport connection status
                    if let Some(ref cm) = contact_mgr {
                        let is_connected = cm.is_connected();
                        let _ = ui_tx.send(UiEvent::TransportConnected(is_connected)).await;
                    }
                    // Send contacts list
                    if let Some(cm) = cm_for_list {
                        for chat_handle in cm.list_contact_chats().await {
                            let contact = chat_handle.contact();
                            let _ = ui_tx
                                .send(UiEvent::ContactAccepted {
                                    address: contact.address.to_string(),
                                    name: contact.local_name.unwrap_or(contact.name),
                                })
                                .await;
                            let _ = ui_tx
                                .send(UiEvent::ContactConnection {
                                    address: contact.address.to_string(),
                                    connected: chat_handle.contact_handle().is_connected(),
                                })
                                .await;
                        }
                    }
                });
                CurrentScreen::Chats(screen)
            }
            ScreenType::Settings { server_addr } => {
                CurrentScreen::Settings(SettingsScreen::new(server_addr).with_theme(self.ctx.theme))
            }
        };

        let focus_task = if let CurrentScreen::Init(screen) = &mut self.screen {
            match screen.focus_initial_field() {
                ScreenCommand::None => None,
                cmd => Some(self.handle_screen_command(cmd, AppMessage::Init)),
            }
        } else {
            None
        };

        match (sync_task, focus_task) {
            (Some(a), Some(b)) => Task::batch(vec![a, b]),
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (None, None) => Task::none(),
        }
    }
}

fn handle_tab_press(
    key: keyboard::Key,
    modifiers: keyboard::Modifiers,
) -> Option<AppMessage> {
    match key {
        keyboard::Key::Named(Named::Tab) => Some(AppMessage::FocusInitField {
            reverse: modifiers.shift(),
        }),
        _ => None,
    }
}

#[derive(Clone)]
pub enum AppMessage {
    // Wrapped screen messages
    Unlock(crate::ui::screens::UnlockMessage),
    Init(crate::ui::screens::InitMessage),
    ChatList(crate::ui::screens::ChatListMessage),
    Settings(crate::ui::screens::SettingsMessage),
    // UI events from subscription
    UiEvent(UiEvent),
    FocusInitField { reverse: bool },
    Tick,
}

impl std::fmt::Debug for AppMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppMessage::Unlock(_) => write!(f, "Unlock(<msg>)"),
            AppMessage::Init(_) => write!(f, "Init(<msg>)"),
            AppMessage::ChatList(_) => write!(f, "ChatList(<msg>)"),
            AppMessage::Settings(_) => write!(f, "Settings(<msg>)"),
            AppMessage::UiEvent(_) => write!(f, "UiEvent(<event>)"),
            AppMessage::FocusInitField { .. } => write!(f, "InitTab"),
            AppMessage::Tick => write!(f, "Tick"),
        }
    }
}

impl ChatApp {
    pub fn new() -> (Self, Task<AppMessage>) {
        let ctx = AppContext::new();
        let theme = ctx.theme.to_iced_theme();
        let mut screen = if ctx.is_initialized() {
            CurrentScreen::Unlock(UnlockScreen::new())
        } else {
            CurrentScreen::Init(InitScreen::with_default_server(DEFAULT_SERVER))
        };
        let focus_task = if let CurrentScreen::Init(init) = &mut screen {
            match init.focus_initial_field() {
                ScreenCommand::None => Task::none(),
                ScreenCommand::Message(task) => task.map(AppMessage::Init),
                ScreenCommand::ChangeScreen(_) => Task::none(),
            }
        } else {
            Task::none()
        };
        (
            Self {
                screen,
                ctx,
                theme,
            },
            focus_task,
        )
    }

    pub fn subscription(&self) -> Subscription<AppMessage> {
        // Create subscriptions vector
        let mut subscriptions = vec![];
        // Add UI event subscription
        let event_rx = self.ctx.ui_event_rx.clone();
        let ui_event_sub = stream::channel(100, move |mut output| async move {
            loop {
                let mut rx = event_rx.lock().await;
                match rx.recv().await {
                    Some(event) => {
                        let _ = output.send(AppMessage::UiEvent(event)).await;
                    }
                    None => {
                        break;
                    }
                }
            }
        });
        subscriptions.push(Subscription::run_with_id(
            TypeId::of::<UiEvent>(),
            ui_event_sub,
        ));
        // Keep tick subscription for compatibility
        subscriptions.push(
            iced::time::every(std::time::Duration::from_millis(250)).map(|_| AppMessage::Tick),
        );
        subscriptions.push(keyboard::on_key_press(handle_tab_press));
        Subscription::batch(subscriptions)
    }

    pub fn title(&self) -> String {
        match self.screen {
            CurrentScreen::Unlock(_) => "ntied: Unlock".to_string(),
            CurrentScreen::Init(_) => "ntied: Init".to_string(),
            CurrentScreen::Chats(_) => "ntied".to_string(),
            CurrentScreen::Settings(_) => "ntied: Settings".to_string(),
        }
    }

    pub fn update(&mut self, message: AppMessage) -> Task<AppMessage> {
        // Update theme if context theme changed
        let new_theme = self.ctx.theme.to_iced_theme();
        if self.theme != new_theme {
            self.theme = new_theme;
        }

        match (&mut self.screen, message) {
            // Handle UI events from subscription
            (_, AppMessage::UiEvent(event)) => {
                match &mut self.screen {
                    CurrentScreen::Chats(screen) => {
                        // Only ChatList screen currently handles UI events
                        screen.apply_event(event.clone());

                        // Save call state to context for preservation across screen switches
                        self.ctx.active_call_address = screen.get_active_call_address();
                        self.ctx.active_call_name = screen.get_active_call_name();
                        self.ctx.active_call_state = screen.get_active_call_state();
                        self.ctx.incoming_call_address = screen.get_incoming_call_address();
                        self.ctx.incoming_call_name = screen.get_incoming_call_name();
                    }
                    _ => {
                        // Other screens don't handle UI events yet
                    }
                }

                // Process specific UI events that need app-level handling
                match event {
                    UiEvent::IncomingCall { address: _ } => {
                        // Start ringtone
                        let ringtone = self.ctx.ringtone_player.clone();
                        return Task::perform(
                            async move {
                                let mut player = ringtone.lock().await;
                                if let Err(e) = player.start() {
                                    tracing::error!("Failed to start ringtone: {}", e);
                                }
                            },
                            |_| AppMessage::Tick,
                        );
                    }
                    UiEvent::CallAccepted { .. }
                    | UiEvent::CallRejected { .. }
                    | UiEvent::CallConnected { .. }
                    | UiEvent::CallEnded { .. } => {
                        // Stop ringtone
                        let ringtone = self.ctx.ringtone_player.clone();
                        return Task::perform(
                            async move {
                                let mut player = ringtone.lock().await;
                                player.stop();
                            },
                            |_| AppMessage::Tick,
                        );
                    }
                    UiEvent::ContactAccepted { name, address } => {
                        let chats = self.ctx.chat_manager.clone();
                        let contacts = self.ctx.contact_manager.clone();
                        return Task::perform(
                            async move {
                                if let (Some(chats), Some(contacts)) = (chats, contacts) {
                                    if let Ok(address) = address.parse() {
                                        let handle = contacts.connect_contact(address).await;
                                        if let Err(err) = chats
                                            .add_contact_chat(
                                                address,
                                                handle.public_key().unwrap(),
                                                name,
                                                None,
                                            )
                                            .await
                                        {
                                            tracing::error!(?err, "Cannot add contact chat");
                                        }
                                    }
                                }
                                ()
                            },
                            |_| AppMessage::Tick,
                        );
                    }
                    UiEvent::ContactRemoved { address } => {
                        let chats = self.ctx.chat_manager.clone();
                        return Task::perform(
                            async move {
                                if let Some(chats) = chats {
                                    if let Ok(address) = address.parse() {
                                        if let Err(err) = chats.remove_contact_chat(address).await {
                                            tracing::error!(?err, "Cannot remove contact chat");
                                        }
                                    }
                                }
                                ()
                            },
                            |_| AppMessage::Tick,
                        );
                    }
                    _ => Task::none(),
                }
            }
            // Unlock screen: use new trait-based approach
            (CurrentScreen::Unlock(u), AppMessage::Unlock(msg)) => {
                let cmd = u.update(msg, &mut self.ctx);
                self.handle_screen_command(cmd, AppMessage::Unlock)
            }
            // Init screen: use new trait-based approach
            (CurrentScreen::Init(i), AppMessage::Init(msg)) => {
                let cmd = i.update(msg, &mut self.ctx);
                self.handle_screen_command(cmd, AppMessage::Init)
            }
            // ChatList screen: use new trait-based approach
            (CurrentScreen::Chats(c), AppMessage::ChatList(msg)) => {
                let cmd = c.update(msg, &mut self.ctx);
                self.handle_screen_command(cmd, AppMessage::ChatList)
            }
            // Settings screen: use new trait-based approach
            (CurrentScreen::Settings(s), AppMessage::Settings(msg)) => {
                let cmd = s.update(msg, &mut self.ctx);
                self.handle_screen_command(cmd, AppMessage::Settings)
            }
            (_, AppMessage::FocusInitField { reverse }) => match &mut self.screen {
                CurrentScreen::Init(screen) => {
                    let cmd = screen.handle_tab_navigation(reverse);
                    self.handle_screen_command(cmd, AppMessage::Init)
                }
                _ => Task::none(),
            },
            // Tick: now mostly for compatibility, UI events handled via subscription
            (_, AppMessage::Tick) => {
                // UI events are now handled through AppMessage::UiEvent
                Task::none()
            }
            // Ignore unmatched pairs
            _ => Task::none(),
        }
    }

    pub fn view(&self) -> Element<'_, AppMessage> {
        match &self.screen {
            CurrentScreen::Unlock(u) => u.view(&self.theme).map(AppMessage::Unlock),
            CurrentScreen::Init(i) => i.view(&self.theme).map(AppMessage::Init),
            CurrentScreen::Chats(c) => c.view(&self.theme).map(AppMessage::ChatList),
            CurrentScreen::Settings(s) => s.view(&self.theme).map(AppMessage::Settings),
        }
    }

    pub fn theme(&self) -> Theme {
        self.theme.clone()
    }
}
