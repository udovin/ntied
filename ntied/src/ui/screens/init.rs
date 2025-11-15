use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;

use iced::widget::{Space, button, column, container, row, text, text_input};
use iced::{Alignment, Element, Length, Task, Theme};
use tokio::sync::{Mutex as TokioMutex, mpsc};

use crate::DEFAULT_SERVER;
use crate::call::CallManager;
use crate::chat::ChatManager;
use crate::config::ConfigManager;
use crate::contact::ContactManager;
use crate::storage::Storage;
use crate::ui::core::{Screen, ScreenCommand, ScreenType};
use crate::ui::screens::unlock::InitSuccess;
use crate::ui::theme::{colors, styles};
use crate::ui::{AppContext, UiEvent, UiEventListener};

/// Init screen module: gathers account data (name, password, server address) and emits messages to the App layer.
/// - Disables the submit button while initialization is in progress (`is_busy`).
/// - Shows inline validation under each field.
/// - Shows a global error if initialization fails.
pub struct InitScreen {
    name: String,
    password: String,
    password_confirm: String,
    server_addr: String,
    is_busy: bool,
    // Per-field errors
    name_error: Option<String>,
    password_error: Option<String>,
    password_confirm_error: Option<String>,
    server_addr_error: Option<String>,
    // Global error (operation failed)
    global_error: Option<String>,
}

/// Messages emitted by the init screen.
#[derive(Debug, Clone)]
pub enum InitMessage {
    /// User typed into the name field.
    NameChanged(String),
    /// User typed into the password field.
    PasswordChanged(String),
    /// User typed into the confirm password field.
    PasswordConfirmChanged(String),
    /// User typed into the server address field.
    ServerAddrChanged(String),
    /// User pressed the create button.
    Submit,
    /// Result of async init operation.
    InitComplete(Result<InitSuccess, String>),
}

impl InitScreen {
    /// Create a new init screen with no pre-filled values.
    pub fn new() -> Self {
        Self {
            name: String::new(),
            password: String::new(),
            password_confirm: String::new(),
            server_addr: String::new(),
            is_busy: false,
            name_error: None,
            password_error: None,
            password_confirm_error: None,
            server_addr_error: None,
            global_error: None,
        }
    }

    /// Create a new init screen with a default server address hint.
    pub fn with_default_server(server: impl Into<String>) -> Self {
        let mut screen = Self::new();
        screen.server_addr = server.into();
        screen
    }

    /// External setter for busy flag (optional convenience).
    pub fn set_busy(&mut self, busy: bool) {
        self.is_busy = busy;
    }

    /// External setter for global error (optional convenience).
    pub fn set_error(&mut self, msg: impl Into<String>) {
        self.global_error = Some(msg.into());
    }

    /// Accessors for current values (optional).
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn password(&self) -> &str {
        &self.password
    }
    pub fn password_confirm(&self) -> &str {
        &self.password_confirm
    }
    pub fn server_addr(&self) -> &str {
        &self.server_addr
    }

    // Validation helpers
    fn validate_name(name: &str) -> Option<String> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Some("Name cannot be empty".into());
        }
        if trimmed.len() > 64 {
            return Some("Name is too long (max 64)".into());
        }
        None
    }

    fn validate_password(pwd: &str) -> Option<String> {
        if pwd.len() < 4 {
            return Some("Password is too short (min 4)".into());
        }
        if pwd.len() > 64 {
            return Some("Password is too long (max 64)".into());
        }
        None
    }

    fn validate_password_confirm(pwd: &str, confirm: &str) -> Option<String> {
        if pwd != confirm {
            return Some("Passwords do not match".into());
        }
        None
    }

    fn validate_server_addr(s: &str) -> Option<String> {
        if s.trim().is_empty() {
            return Some("Server address cannot be empty".into());
        }
        match s.parse::<std::net::SocketAddr>() {
            Ok(_) => None,
            Err(_) => Some("Invalid server address (expected host:port)".into()),
        }
    }

    // Inline error renderer
    fn inline_error<'a>(err: Option<&'a str>, theme: &Theme) -> Element<'a, InitMessage> {
        if let Some(e) = err {
            container(text(e).size(14).color(colors::text_error(theme)))
                .padding([2, 4])
                .into()
        } else {
            Space::with_height(0).into()
        }
    }
}

impl Screen for InitScreen {
    type Message = InitMessage;

    fn update(&mut self, message: InitMessage, ctx: &mut AppContext) -> ScreenCommand<InitMessage> {
        match message {
            InitMessage::NameChanged(value) => {
                self.name = value;
                self.name_error = Self::validate_name(&self.name);
                self.global_error = None;
                ScreenCommand::None
            }
            InitMessage::PasswordChanged(value) => {
                self.password = value;
                self.password_error = Self::validate_password(&self.password);
                // Keep confirm validation in sync
                self.password_confirm_error =
                    Self::validate_password_confirm(&self.password, &self.password_confirm);
                self.global_error = None;
                ScreenCommand::None
            }
            InitMessage::PasswordConfirmChanged(value) => {
                self.password_confirm = value;
                self.password_confirm_error =
                    Self::validate_password_confirm(&self.password, &self.password_confirm);
                self.global_error = None;
                ScreenCommand::None
            }
            InitMessage::ServerAddrChanged(value) => {
                self.server_addr = value;
                self.server_addr_error = Self::validate_server_addr(&self.server_addr);
                self.global_error = None;
                ScreenCommand::None
            }
            InitMessage::Submit => {
                // Re-validate before submitting
                self.name_error = Self::validate_name(&self.name);
                self.password_error = Self::validate_password(&self.password);
                self.password_confirm_error =
                    Self::validate_password_confirm(&self.password, &self.password_confirm);
                self.server_addr_error = Self::validate_server_addr(&self.server_addr);

                // Check if there are any errors
                if self.name_error.is_some()
                    || self.password_error.is_some()
                    || self.password_confirm_error.is_some()
                    || self.server_addr_error.is_some()
                {
                    return ScreenCommand::None;
                }

                // Set busy and start async init flow
                self.is_busy = true;
                let path = ctx.storage_dir.clone();
                let name = self.name.clone();
                let password = self.password.clone();
                let server_addr_str = self.server_addr.clone();
                let ui_event_tx = ctx.ui_event_tx.clone();

                let cmd = Task::perform(
                    init_flow(path, name, password, server_addr_str, ui_event_tx),
                    InitMessage::InitComplete,
                );
                ScreenCommand::Message(cmd)
            }
            InitMessage::InitComplete(result) => {
                self.is_busy = false;
                match result {
                    Ok(success) => {
                        let own_name = success.profile.name.clone();
                        let own_address = success.contact_manager.get_own_address().to_string();
                        tracing::info!(?own_address, ?own_name, "Successfully initialized");
                        // Store context from init
                        ctx.storage = Some(success.storage.clone());
                        ctx.chat_manager = Some(success.chat_manager);
                        ctx.contact_manager = Some(success.contact_manager.clone());
                        ctx.call_manager = Some(success.call_manager.clone());
                        ctx.profile = Some(success.profile.clone());
                        ctx.server_addr = Some(success.server_addr);
                        // Initialize contacts list (usually empty for new account) and connection status
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

    fn view<'a>(&'a self, theme: &'a Theme) -> Element<'a, InitMessage> {
        let header = row![
            text("Create Account").size(28),
            Space::with_width(Length::Fill),
        ]
        .align_y(Alignment::Center);
        // Name field
        let name_label = text("Name").size(16);
        let name_input = text_input("Enter display name", &self.name)
            .on_input(InitMessage::NameChanged)
            .padding(10)
            .size(16)
            .width(Length::Fixed(360.0));
        let name_error = Self::inline_error(self.name_error.as_deref(), theme);
        // Password field
        let password_label = text("Password").size(16);
        let password_input = text_input("Enter password", &self.password)
            .on_input(InitMessage::PasswordChanged)
            .secure(true)
            .padding(10)
            .size(16)
            .width(Length::Fixed(360.0));
        let password_error = Self::inline_error(self.password_error.as_deref(), theme);
        // Confirm password field
        let confirm_label = text("Confirm Password").size(16);
        let confirm_input = text_input("Re-enter password", &self.password_confirm)
            .on_input(InitMessage::PasswordConfirmChanged)
            .secure(true)
            .padding(10)
            .size(16)
            .width(Length::Fixed(360.0));
        let confirm_error = Self::inline_error(self.password_confirm_error.as_deref(), theme);
        // Server address field
        let server_label = text("Server Address").size(16);
        let server_input = text_input(&format!("e.g., {DEFAULT_SERVER}"), &self.server_addr)
            .on_input(InitMessage::ServerAddrChanged)
            .padding(10)
            .size(16)
            .width(Length::Fixed(360.0));
        let server_error = Self::inline_error(self.server_addr_error.as_deref(), theme);
        let can_submit = !self.is_busy
            && self.name_error.is_none()
            && self.password_error.is_none()
            && self.password_confirm_error.is_none()
            && self.server_addr_error.is_none()
            && !self.name.trim().is_empty()
            && !self.password.trim().is_empty()
            && !self.password_confirm.trim().is_empty()
            && !self.server_addr.trim().is_empty();
        let mut submit_button = button(if self.is_busy {
            "Creating..."
        } else {
            "Create"
        })
        .padding([10, 20])
        .style(button::primary);
        if can_submit {
            submit_button = submit_button.on_press(InitMessage::Submit);
        }
        let mut content = column![
            header,
            Space::with_height(16),
            container(
                column![
                    name_label,
                    name_input,
                    name_error,
                    Space::with_height(8),
                    password_label,
                    password_input,
                    password_error,
                    Space::with_height(8),
                    confirm_label,
                    confirm_input,
                    confirm_error,
                    Space::with_height(8),
                    server_label,
                    server_input,
                    server_error,
                    Space::with_height(16),
                    submit_button,
                ]
                .spacing(8)
            )
            .padding(20)
            .style(move |t: &Theme| styles::card(t)),
        ]
        .spacing(10)
        .padding(20)
        .width(Length::Fixed(600.0));
        if let Some(err) = &self.global_error {
            content = content.push(Space::with_height(10));
            content = content.push(
                container(text(err).color(colors::text_error(theme)))
                    .padding(10)
                    .width(Length::Fill)
                    .style(move |t: &Theme| styles::error_text(t)),
            );
        }
        container(content)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into()
    }
}

/// Async flow for initializing new account
async fn init_flow(
    path: std::path::PathBuf,
    name: String,
    password: String,
    server_addr_str: String,
    ui_event_tx: mpsc::Sender<UiEvent>,
) -> Result<InitSuccess, String> {
    // Create storage
    std::fs::create_dir_all(&path).map_err(|e| format!("Failed to create data dir: {}", e))?;
    let storage = Storage::create(&path, &password)
        .await
        .map_err(|e| format!("Failed to create storage: {}", e))?;
    let storage = Arc::new(TokioMutex::new(storage));
    let cfg = ConfigManager::new(storage.clone());
    // Initialize account: generates keypair, stores private key and profile
    let (profile, private_key) = cfg
        .init_account(name)
        .await
        .map_err(|e| format!("Failed to initialize account: {}", e))?;
    // Validate and store server address
    let server_addr = SocketAddr::from_str(&server_addr_str)
        .map_err(|e| format!("Invalid server address '{}': {}", server_addr_str, e))?;
    cfg.set_server_addr(server_addr)
        .await
        .map_err(|e| format!("Failed to save server address: {}", e))?;
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
        chat_manager,
        contact_manager,
        call_manager,
        profile,
        server_addr,
    })
}
