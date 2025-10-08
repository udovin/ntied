use std::sync::Arc;

use iced::widget::{Space, button, column, container, row, text, text_input};
use iced::{Alignment, Color, Element, Length, Task};
use tokio::sync::{Mutex as TokioMutex, mpsc};

use crate::call::CallManager;
use crate::chat::ChatManager;
use crate::config::ConfigManager;
use crate::contact::ContactManager;
use crate::packet::ContactProfile;
use crate::storage::Storage;
use crate::ui::core::{Screen, ScreenCommand, ScreenType};
use crate::ui::{AppContext, UiEvent, UiEventListener};

/// Unlock screen module: gathers a password and emits messages to the App layer.
/// - Disables the submit button while unlocking (`is_busy`).
/// - Shows inline validation under the password field.
/// - Shows a global error if unlock fails.
pub struct UnlockScreen {
    password: String,
    is_busy: bool,
    password_error: Option<String>,
    global_error: Option<String>,
}

/// Messages emitted by the unlock screen.
#[derive(Debug, Clone)]
pub enum UnlockMessage {
    /// User typed into the password field.
    PasswordChanged(String),
    /// User pressed the unlock button.
    Submit,
    /// Result of async unlock operation.
    UnlockComplete(Result<InitSuccess, String>),
}

/// Success result from unlock operation.
#[derive(Clone)]
pub struct InitSuccess {
    pub storage: Arc<TokioMutex<Storage>>,
    pub contact_manager: Arc<ContactManager>,
    pub chat_manager: Arc<ChatManager>,
    pub call_manager: Arc<CallManager>,
    pub profile: ContactProfile,
    pub server_addr: std::net::SocketAddr,
}

impl std::fmt::Debug for InitSuccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InitSuccess")
            .field("storage", &"Arc<Mutex<Storage>>")
            .field("contact_manager", &"Arc<ContactManager>")
            .field("chat_manager", &"Arc<ChatManager>")
            .field("call_manager", &"Arc<CallManager>")
            .field("profile", &self.profile)
            .field("server_addr", &self.server_addr)
            .finish()
    }
}

impl UnlockScreen {
    /// Create a new unlock screen with empty password and no errors.
    pub fn new() -> Self {
        Self {
            password: String::new(),
            is_busy: false,
            password_error: None,
            global_error: None,
        }
    }

    /// External setter for busy flag (optional convenience).
    pub fn set_busy(&mut self, busy: bool) {
        self.is_busy = busy;
    }

    /// External setter for global error (optional convenience).
    pub fn set_error(&mut self, msg: impl Into<String>) {
        self.global_error = Some(msg.into());
    }

    /// Accessor for current password (optional).
    pub fn password(&self) -> &str {
        &self.password
    }

    /// Simple password validation mirroring storage constraints.
    fn validate_password(pwd: &str) -> Option<String> {
        if pwd.len() < 4 {
            return Some("Password is too short (min 4)".into());
        }
        if pwd.len() > 64 {
            return Some("Password is too long (max 64)".into());
        }
        None
    }
}

impl Screen for UnlockScreen {
    type Message = UnlockMessage;

    fn update(
        &mut self,
        message: UnlockMessage,
        ctx: &mut AppContext,
    ) -> ScreenCommand<UnlockMessage> {
        match message {
            UnlockMessage::PasswordChanged(value) => {
                self.password = value;
                self.password_error = Self::validate_password(&self.password);
                self.global_error = None;
                ScreenCommand::None
            }
            UnlockMessage::Submit => {
                // Re-validate before submitting
                self.password_error = Self::validate_password(&self.password);
                if self.password_error.is_some() {
                    return ScreenCommand::None;
                }
                // Set busy and start async unlock flow
                self.is_busy = true;
                let path = ctx.storage_dir.clone();
                let password = self.password.clone();
                let ui_event_tx = ctx.ui_event_tx.clone();

                let cmd = Task::perform(
                    unlock_flow(path, password, ui_event_tx),
                    UnlockMessage::UnlockComplete,
                );
                ScreenCommand::Message(cmd)
            }
            UnlockMessage::UnlockComplete(result) => {
                self.is_busy = false;
                match result {
                    Ok(success) => {
                        let own_name = success.profile.name.clone();
                        let own_address = success.contact_manager.get_own_address().to_string();
                        tracing::info!(?own_address, ?own_name, "Successfully unlocked");
                        // Store context from unlock
                        ctx.storage = Some(success.storage.clone());
                        ctx.chat_manager = Some(success.chat_manager);
                        ctx.contact_manager = Some(success.contact_manager.clone());
                        ctx.call_manager = Some(success.call_manager.clone());
                        ctx.profile = Some(success.profile.clone());
                        ctx.server_addr = Some(success.server_addr);
                        // Initialize contacts list and connection status
                        let ui_tx = ctx.ui_event_tx.clone();
                        let cm_for_list = ctx.chat_manager.clone();
                        let contact_mgr = ctx.contact_manager.clone();
                        tokio::spawn(async move {
                            // Send transport connection status
                            if let Some(ref cm) = contact_mgr {
                                let is_connected = cm.is_connected();
                                let _ = ui_tx.send(UiEvent::TransportConnected(is_connected)).await;
                            }
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
                        // Switch to chats screen
                        ScreenCommand::ChangeScreen(ScreenType::Chats {
                            own_name,
                            own_address,
                        })
                    }
                    Err(error) => {
                        self.global_error = Some(error);
                        ScreenCommand::None
                    }
                }
            }
        }
    }

    fn view(&self) -> Element<'_, UnlockMessage> {
        let header = row![text("Unlock").size(28), Space::with_width(Length::Fill),]
            .align_y(Alignment::Center);
        let password_label = text("Password").size(16);
        let password_input = text_input("Enter password", &self.password)
            .on_input(UnlockMessage::PasswordChanged)
            .secure(true)
            .padding(10)
            .size(16)
            .width(Length::Fixed(360.0));
        let inline_error: Element<_> = if let Some(err) = &self.password_error {
            container(text(err).size(14))
                .padding([2, 4])
                .style(|_theme: &iced::Theme| container::Style {
                    text_color: Some(iced::Color::from_rgb(0.85, 0.2, 0.2)),
                    ..Default::default()
                })
                .into()
        } else {
            Space::with_height(0).into()
        };
        let can_submit =
            !self.is_busy && self.password_error.is_none() && !self.password.trim().is_empty();
        let mut unlock_button = button(if self.is_busy {
            "Unlocking..."
        } else {
            "Unlock"
        })
        .padding([10, 20])
        .style(button::primary);
        if can_submit {
            unlock_button = unlock_button.on_press(UnlockMessage::Submit);
        }
        let mut content = column![
            header,
            Space::with_height(16),
            container(
                column![
                    password_label,
                    password_input,
                    inline_error,
                    Space::with_height(12),
                    unlock_button,
                ]
                .spacing(8)
            )
            .padding(20)
            .style(|theme: &iced::Theme| {
                let palette = theme.extended_palette();
                container::Style {
                    background: Some(iced::Background::Color(palette.background.weak.color)),
                    border: iced::Border {
                        color: palette.background.strong.color,
                        width: 1.0,
                        radius: 8.0.into(),
                    },
                    ..Default::default()
                }
            }),
        ]
        .spacing(10)
        .padding(20)
        .width(Length::Fixed(600.0));
        if let Some(err) = &self.global_error {
            content = content.push(Space::with_height(10));
            content = content.push(
                container(text(err).color(Color::from_rgb(0.9, 0.3, 0.3)))
                    .padding(10)
                    .width(Length::Fill),
            );
        }
        container(content)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into()
    }
}

/// Async flow for unlocking storage
async fn unlock_flow(
    path: std::path::PathBuf,
    password: String,
    ui_event_tx: mpsc::Sender<UiEvent>,
) -> Result<InitSuccess, String> {
    let storage = Storage::open(&path, &password)
        .await
        .map_err(|e| format!("Failed to unlock: {}", e))?;
    let storage = Arc::new(TokioMutex::new(storage));
    let cfg = ConfigManager::new(storage.clone());
    let profile = cfg.get_profile().await.map_err(|v| v.to_string())?;
    let server_addr = cfg.get_server_addr().await.map_err(|v| v.to_string())?;
    let private_key = cfg.get_private_key().await.map_err(|v| v.to_string())?;
    let listener = Arc::new(UiEventListener::new(ui_event_tx.clone()));
    let contact_manager = Arc::new(
        ContactManager::with_listener(server_addr, private_key, profile.clone(), listener.clone())
            .await,
    );
    let chat_manager = Arc::new(
        ChatManager::with_listener(storage.clone(), contact_manager.clone(), listener.clone())
            .await
            .map_err(|e| format!("ChatManager init failed: {}", e))?,
    );
    let call_manager = CallManager::with_listener(contact_manager.clone(), listener.clone());
    Ok(InitSuccess {
        storage,
        contact_manager,
        chat_manager,
        call_manager,
        profile,
        server_addr,
    })
}
