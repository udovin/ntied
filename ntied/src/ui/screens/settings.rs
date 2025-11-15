use std::net::SocketAddr;
use std::str::FromStr as _;

use iced::widget::{Space, button, column, container, row, scrollable, text, text_input};
use iced::{Alignment, Element, Length, Padding, Task, Theme};

use crate::config::ConfigManager;
use crate::ui::core::{Screen, ScreenCommand, ScreenType};
use crate::ui::theme::{ThemePreference, colors};
use crate::ui::{AppContext, UiEvent};

#[derive(Clone, Debug)]
pub enum SettingsMessage {
    ServerAddressChanged(String),
    ThemeChanged(ThemePreference),
    SaveSettings,
    CancelSettings,
    ResetToDefault,
    SaveComplete(Result<(), String>),
}

pub struct SettingsScreen {
    server_address: String,
    original_server_address: String,
    theme: ThemePreference,
    original_theme: ThemePreference,
    has_changes: bool,
    error_message: Option<String>,
}

impl SettingsScreen {
    pub fn new(current_server: String) -> Self {
        Self {
            server_address: current_server.clone(),
            original_server_address: current_server,
            theme: ThemePreference::default(),
            original_theme: ThemePreference::default(),
            has_changes: false,
            error_message: None,
        }
    }

    pub fn with_theme(mut self, theme: ThemePreference) -> Self {
        self.theme = theme;
        self.original_theme = theme;
        self
    }

    pub fn server_address(&self) -> &str {
        &self.server_address
    }

    pub fn has_changes(&self) -> bool {
        self.has_changes
    }

    fn update_internal(&mut self, message: SettingsMessage) -> Task<SettingsMessage> {
        match message {
            SettingsMessage::ServerAddressChanged(value) => {
                self.server_address = value;
                self.has_changes = self.server_address != self.original_server_address
                    || self.theme != self.original_theme;
                self.error_message = self.validate_server_address();
                Task::none()
            }
            SettingsMessage::ThemeChanged(new_theme) => {
                self.theme = new_theme;
                self.has_changes = self.server_address != self.original_server_address
                    || self.theme != self.original_theme;
                Task::none()
            }
            SettingsMessage::SaveSettings => {
                if self.validate_server_address().is_none() {
                    self.original_server_address = self.server_address.clone();
                    self.original_theme = self.theme;
                    self.has_changes = false;
                    // Parent will handle actual saving
                }
                Task::none()
            }
            SettingsMessage::CancelSettings => {
                // Revert to original
                self.server_address = self.original_server_address.clone();
                self.theme = self.original_theme;
                self.has_changes = false;
                self.error_message = None;
                Task::none()
            }
            SettingsMessage::SaveComplete(_) => {
                // Handled in Screen trait implementation
                Task::none()
            }
            SettingsMessage::ResetToDefault => {
                self.server_address = crate::DEFAULT_SERVER.to_string();
                self.theme = ThemePreference::default();
                self.has_changes = self.server_address != self.original_server_address
                    || self.theme != self.original_theme;
                self.error_message = self.validate_server_address();
                Task::none()
            }
        }
    }

    fn validate_server_address(&self) -> Option<String> {
        let trimmed = self.server_address.trim();
        if trimmed.is_empty() {
            return Some("Server address cannot be empty".to_string());
        }
        // Try to parse as socket address
        if SocketAddr::from_str(trimmed).is_err() {
            // Also try with default port if not specified
            if !trimmed.contains(':') {
                let with_port = format!("{}:9001", trimmed);
                if SocketAddr::from_str(&with_port).is_err() {
                    return Some(
                        "Invalid server address format (e.g., 127.0.0.1:9001)".to_string(),
                    );
                }
            } else {
                return Some("Invalid server address format".to_string());
            }
        }

        None
    }

    pub fn view<'a>(&'a self, theme: &'a Theme) -> Element<'a, SettingsMessage> {
        let header = container(
            row![text("Settings").size(24), Space::with_width(Length::Fill),]
                .align_y(Alignment::Center),
        )
        .width(Length::Fill)
        .padding(Padding::ZERO.bottom(16));
        // Server section
        let server_section = container(
            column![
                text("Connection").size(18),
                Space::with_height(12),
                text("Server Address").size(14),
                Space::with_height(4),
                text_input("e.g., 127.0.0.1:9001", &self.server_address)
                    .on_input(SettingsMessage::ServerAddressChanged)
                    .padding(10)
                    .size(14)
                    .width(Length::Fixed(300.0)),
                if let Some(ref error) = self.error_message {
                    Element::from(
                        container(text(error).size(12).color(colors::text_error(theme)))
                            .padding(Padding::from([4, 0])),
                    )
                } else {
                    Element::from(Space::with_height(0))
                },
                Space::with_height(8),
                text("Default server is used for initial connection")
                    .size(12)
                    .color(colors::text_secondary(theme)),
            ]
            .spacing(4),
        )
        .padding(Padding::from([16, 0]))
        .width(Length::Fill);

        // Appearance section
        let appearance_section = container(
            column![
                Space::with_height(24),
                text("Appearance").size(18),
                Space::with_height(12),
                text("Theme").size(14),
                Space::with_height(4),
                row![
                    button(
                        text(if self.theme == ThemePreference::Light {
                            "● Light"
                        } else {
                            "○ Light"
                        })
                        .size(14)
                    )
                    .on_press(SettingsMessage::ThemeChanged(ThemePreference::Light))
                    .padding([8, 16])
                    .style(if self.theme == ThemePreference::Light {
                        button::primary
                    } else {
                        button::secondary
                    }),
                    Space::with_width(8),
                    button(
                        text(if self.theme == ThemePreference::Dark {
                            "● Dark"
                        } else {
                            "○ Dark"
                        })
                        .size(14)
                    )
                    .on_press(SettingsMessage::ThemeChanged(ThemePreference::Dark))
                    .padding([8, 16])
                    .style(if self.theme == ThemePreference::Dark {
                        button::primary
                    } else {
                        button::secondary
                    }),
                ]
                .spacing(0),
            ]
            .spacing(4),
        )
        .padding(Padding::from([16, 0]))
        .width(Length::Fill);

        // Future settings sections placeholder
        let future_section = container(column![
            Space::with_height(24),
            text("Audio").size(18),
            Space::with_height(8),
            text("Audio device preferences will be saved here in future updates")
                .size(12)
                .color(colors::text_secondary(theme)),
        ])
        .padding(Padding::ZERO)
        .width(Length::Fill);
        // Action buttons
        let actions = container(
            row![
                button(text("Reset to Default").size(14))
                    .on_press(SettingsMessage::ResetToDefault)
                    .padding([6, 12])
                    .style(button::secondary),
                Space::with_width(Length::Fill),
                button(text("Cancel").size(14))
                    .on_press(SettingsMessage::CancelSettings)
                    .padding([6, 12])
                    .style(button::secondary),
                Space::with_width(8),
                button(text("Save").size(14))
                    .on_press_maybe(if self.has_changes && self.error_message.is_none() {
                        Some(SettingsMessage::SaveSettings)
                    } else {
                        None
                    })
                    .padding([6, 12])
                    .style(if self.has_changes && self.error_message.is_none() {
                        button::primary
                    } else {
                        button::secondary
                    }),
            ]
            .align_y(Alignment::Center),
        )
        .width(Length::Fill)
        .padding(Padding::ZERO.top(16));
        let content = column![
            header,
            scrollable(column![server_section, appearance_section, future_section,].spacing(0))
                .height(Length::Fill),
            actions,
        ]
        .spacing(0);
        container(content)
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(20)
            .into()
    }
}

impl Screen for SettingsScreen {
    type Message = SettingsMessage;

    fn update(
        &mut self,
        message: SettingsMessage,
        ctx: &mut AppContext,
    ) -> ScreenCommand<SettingsMessage> {
        match message {
            SettingsMessage::SaveSettings => {
                if self.validate_server_address().is_none() {
                    let new_server = self.server_address.clone();
                    let new_theme = self.theme;

                    // Update theme in context
                    ctx.theme = new_theme;

                    // Parse and validate the address
                    if let Ok(addr) = std::net::SocketAddr::from_str(&new_server) {
                        // Check if server address actually changed
                        let old_addr = ctx.server_addr;
                        ctx.server_addr = Some(addr);
                        // Update ContactManager if address changed
                        if old_addr != Some(addr) {
                            if let Some(ref contact_mgr) = ctx.contact_manager {
                                let cm = contact_mgr.clone();
                                let new_addr = addr;
                                tokio::spawn(async move {
                                    if let Err(err) = cm.change_server_addr(new_addr).await {
                                        tracing::error!(
                                            "Failed to update ContactManager server address: {}",
                                            err
                                        );
                                    } else {
                                        tracing::info!(
                                            "Updated ContactManager server address to: {}",
                                            new_addr
                                        );
                                    }
                                });
                            }
                        }
                        // Save to persistent storage
                        if let Some(ref storage) = ctx.storage {
                            let config_mgr = ConfigManager::new(storage.clone());
                            let addr_clone = addr;
                            let cmd = Task::perform(
                                async move {
                                    if let Err(e) = config_mgr.set_server_addr(addr_clone).await {
                                        Err(format!("Failed to save server address: {}", e))
                                    } else {
                                        Ok(())
                                    }
                                },
                                SettingsMessage::SaveComplete,
                            );
                            self.original_server_address = self.server_address.clone();
                            self.has_changes = false;
                            return ScreenCommand::Message(cmd);
                        }
                    }
                }
                ScreenCommand::None
            }
            SettingsMessage::SaveComplete(result) => {
                if let Err(error) = result {
                    self.error_message = Some(error);
                } else {
                    // Send updated connection status
                    if let Some(ref contact_mgr) = ctx.contact_manager {
                        let is_connected = contact_mgr.is_connected();
                        let ui_tx = ctx.ui_event_tx.clone();
                        tokio::spawn(async move {
                            let _ = ui_tx.send(UiEvent::TransportConnected(is_connected)).await;
                        });
                    }
                    // Return to chat screen after successful save
                    if let Some(ref profile) = ctx.profile {
                        let own_name = profile.name.clone();
                        let own_address = ctx
                            .contact_manager
                            .as_ref()
                            .map(|cm| cm.get_own_address().to_string())
                            .unwrap_or_default();
                        return ScreenCommand::ChangeScreen(ScreenType::Chats {
                            own_name,
                            own_address,
                        });
                    }
                }
                ScreenCommand::None
            }
            SettingsMessage::CancelSettings => {
                // Reset and return to chat screen
                self.server_address = self.original_server_address.clone();
                self.theme = self.original_theme;
                self.has_changes = false;
                self.error_message = None;
                // Send updated connection status
                if let Some(ref contact_mgr) = ctx.contact_manager {
                    let is_connected = contact_mgr.is_connected();
                    let ui_tx = ctx.ui_event_tx.clone();
                    tokio::spawn(async move {
                        let _ = ui_tx.send(UiEvent::TransportConnected(is_connected)).await;
                    });
                }
                if let Some(ref profile) = ctx.profile {
                    let own_name = profile.name.clone();
                    let own_address = ctx
                        .contact_manager
                        .as_ref()
                        .map(|cm| cm.get_own_address().to_string())
                        .unwrap_or_default();
                    return ScreenCommand::ChangeScreen(ScreenType::Chats {
                        own_name,
                        own_address,
                    });
                }
                ScreenCommand::None
            }
            _ => {
                // Use internal update for other messages
                let cmd = self.update_internal(message);
                ScreenCommand::Message(cmd)
            }
        }
    }

    fn view<'a>(&'a self, theme: &'a Theme) -> Element<'a, SettingsMessage> {
        self.view(theme)
    }
}
