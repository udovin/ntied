use iced::{Element, Task, Theme};
use std::fmt::Debug;

use crate::ui::{AppContext, UiEvent};

/// Command returned from screen update methods
pub enum ScreenCommand<M> {
    /// No action needed
    None,
    /// Execute a command with screen's message type
    Message(Task<M>),
    /// Switch to a different screen
    ChangeScreen(ScreenType),
}

/// Types of screens for navigation
#[derive(Debug, Clone)]
pub enum ScreenType {
    /// Unlock screen with password input
    Unlock,
    /// Initialization screen for new account
    Init,
    /// Main chat list screen
    Chats {
        own_name: String,
        own_address: String,
    },
    /// Settings screen
    Settings { server_addr: String },
}

/// Base trait for all application screens
pub trait Screen {
    /// Message type for this screen
    type Message: Debug + Clone + Send + 'static;

    /// Process a screen message and return a command
    fn update(
        &mut self,
        message: Self::Message,
        ctx: &mut AppContext,
    ) -> ScreenCommand<Self::Message>;

    /// Handle UI events from background managers
    /// Default implementation ignores all events
    fn handle_ui_event(
        &mut self,
        _event: UiEvent,
        _ctx: &mut AppContext,
    ) -> ScreenCommand<Self::Message> {
        ScreenCommand::None
    }

    /// Create the view for this screen
    fn view<'a>(&'a self, theme: &'a Theme) -> Element<'a, Self::Message>;
}

/// Helper methods for ScreenCommand
impl<M> ScreenCommand<M> {
    /// Check if command requests screen change
    pub fn get_screen_change(&self) -> Option<ScreenType> {
        match self {
            ScreenCommand::ChangeScreen(screen_type) => Some(screen_type.clone()),
            _ => None,
        }
    }

    /// Check if this is a None command
    pub fn is_none(&self) -> bool {
        matches!(self, ScreenCommand::None)
    }
}
