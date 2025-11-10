use std::any::TypeId;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;
use iced::futures::sink::SinkExt as _;
use iced::{Element, Subscription, Task, stream};
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
use crate::ui::screens::{ChatListScreen, InitScreen, SettingsScreen, UnlockScreen};

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
            ScreenCommand::ChangeScreen(screen_type) => {
                self.switch_screen(screen_type);
                Task::none()
            }
        }
    }

    fn switch_screen(&mut self, screen_type: crate::ui::core::ScreenType) {
        self.screen = match screen_type {
            ScreenType::Unlock => CurrentScreen::Unlock(UnlockScreen::new()),
            ScreenType::Init => CurrentScreen::Init(InitScreen::new()),
            ScreenType::Chats {
                own_name,
                own_address,
            } => {
                let mut screen = ChatListScreen::new(Some(own_name.clone()));
                screen.set_identity(own_name, own_address);
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
                CurrentScreen::Settings(SettingsScreen::new(server_addr))
            }
        };
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
            AppMessage::Tick => write!(f, "Tick"),
        }
    }
}

impl ChatApp {
    pub fn new() -> (Self, Task<AppMessage>) {
        let ctx = AppContext::new();
        let screen = if ctx.is_initialized() {
            CurrentScreen::Unlock(UnlockScreen::new())
        } else {
            CurrentScreen::Init(InitScreen::with_default_server(DEFAULT_SERVER))
        };
        (Self { screen, ctx }, Task::none())
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
        match (&mut self.screen, message) {
            // Handle UI events from subscription
            (_, AppMessage::UiEvent(event)) => {
                match &mut self.screen {
                    CurrentScreen::Chats(screen) => {
                        // Only ChatList screen currently handles UI events
                        screen.apply_event(event.clone());
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
            CurrentScreen::Unlock(u) => u.view().map(AppMessage::Unlock),
            CurrentScreen::Init(i) => i.view().map(AppMessage::Init),
            CurrentScreen::Chats(c) => c.view().map(AppMessage::ChatList),
            CurrentScreen::Settings(s) => s.view().map(AppMessage::Settings),
        }
    }
}
