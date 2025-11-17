//! Theme management for the ntied UI
//!
//! This module provides a centralized theme system that supports:
//! - Light and dark themes
//! - Consistent styling across all UI components
//! - Easy customization and extension

use iced::widget::{button, container};
use iced::{Color, Theme};
use serde::{Deserialize, Serialize};

/// Theme preference that can be stored in config
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThemePreference {
    Light,
    Dark,
}

impl Default for ThemePreference {
    fn default() -> Self {
        Self::Light
    }
}

impl ThemePreference {
    pub fn to_iced_theme(self) -> Theme {
        match self {
            Self::Light => Theme::CatppuccinLatte,
            Self::Dark => Theme::CatppuccinMocha,
        }
    }

    pub fn toggle(self) -> Self {
        match self {
            Self::Light => Self::Dark,
            Self::Dark => Self::Light,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }
}

/// Custom styles for various UI components
pub mod styles {
    use super::*;

    /// Style for the main panel divider
    pub fn divider(theme: &Theme) -> container::Style {
        let palette = theme.extended_palette();
        container::Style {
            background: Some(iced::Background::Color(palette.background.strong.color)),
            ..Default::default()
        }
    }

    /// Style for the left panel header (account info)
    pub fn panel_header(theme: &Theme) -> container::Style {
        let palette = theme.extended_palette();
        container::Style {
            background: Some(iced::Background::Color(palette.background.weak.color)),
            ..Default::default()
        }
    }

    /// Style for card containers (modals, forms, etc)
    pub fn card(theme: &Theme) -> container::Style {
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
    }

    /// Style for modal overlay background
    pub fn modal_overlay(theme: &Theme) -> container::Style {
        let palette = theme.extended_palette();
        let mut base_color = palette.background.base.color;
        base_color.a = 0.85;
        container::Style {
            background: Some(iced::Background::Color(base_color)),
            ..Default::default()
        }
    }

    /// Style for connection status indicator (connected)
    pub fn status_connected(theme: &Theme) -> container::Style {
        let palette = theme.extended_palette();
        container::Style {
            background: Some(iced::Background::Color(palette.success.strong.color)),
            border: iced::Border {
                color: palette.success.base.color,
                width: 2.0,
                radius: 6.0.into(),
            },
            ..Default::default()
        }
    }

    /// Style for connection status indicator (disconnected)
    pub fn status_disconnected(theme: &Theme) -> container::Style {
        let palette = theme.extended_palette();
        container::Style {
            background: Some(iced::Background::Color(Color::TRANSPARENT)),
            border: iced::Border {
                color: palette.background.strong.color,
                width: 2.0,
                radius: 6.0.into(),
            },
            ..Default::default()
        }
    }

    /// Style for selected contact in list
    pub fn contact_selected(theme: &Theme) -> container::Style {
        let palette = theme.extended_palette();
        container::Style {
            background: Some(iced::Background::Color(palette.primary.weak.color)),
            border: iced::Border {
                color: palette.primary.strong.color,
                width: 1.0,
                radius: 4.0.into(),
            },
            ..Default::default()
        }
    }

    /// Style for unselected contact in list
    pub fn contact_unselected(_theme: &Theme) -> container::Style {
        container::Style {
            background: Some(iced::Background::Color(Color::TRANSPARENT)),
            border: iced::Border {
                color: Color::TRANSPARENT,
                width: 1.0,
                radius: 4.0.into(),
            },
            ..Default::default()
        }
    }

    /// Style for incoming message bubble
    pub fn message_incoming(theme: &Theme) -> container::Style {
        let palette = theme.extended_palette();
        container::Style {
            background: Some(iced::Background::Color(palette.background.strong.color)),
            border: iced::Border {
                radius: 12.0.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// Style for outgoing message bubble
    pub fn message_outgoing(theme: &Theme) -> container::Style {
        let palette = theme.extended_palette();
        container::Style {
            background: Some(iced::Background::Color(palette.primary.base.color)),
            border: iced::Border {
                radius: 12.0.into(),
                ..Default::default()
            },
            text_color: Some(palette.primary.base.text),
            ..Default::default()
        }
    }

    /// Style for pending/delivering message bubble
    pub fn message_pending(theme: &Theme) -> container::Style {
        let palette = theme.extended_palette();
        let mut bg_color = palette.primary.base.color;
        bg_color.a = 0.6;
        container::Style {
            background: Some(iced::Background::Color(bg_color)),
            border: iced::Border {
                radius: 12.0.into(),
                ..Default::default()
            },
            text_color: Some(palette.primary.base.text),
            ..Default::default()
        }
    }

    /// Style for error messages
    pub fn error_text(theme: &Theme) -> container::Style {
        let palette = theme.extended_palette();
        container::Style {
            text_color: Some(palette.danger.strong.color),
            ..Default::default()
        }
    }

    /// Style for muted/secondary text
    pub fn muted_text(theme: &Theme) -> container::Style {
        let palette = theme.extended_palette();
        container::Style {
            text_color: Some(palette.background.strong.text),
            ..Default::default()
        }
    }

    /// Style for the chat header
    pub fn chat_header(theme: &Theme) -> container::Style {
        let palette = theme.extended_palette();
        container::Style {
            background: Some(iced::Background::Color(palette.background.weak.color)),
            border: iced::Border {
                color: palette.background.strong.color,
                width: 0.0,
                radius: 0.0.into(),
            },
            ..Default::default()
        }
    }

    /// Style for incoming call notification
    pub fn incoming_call(theme: &Theme) -> container::Style {
        let palette = theme.extended_palette();
        container::Style {
            background: Some(iced::Background::Color(palette.success.weak.color)),
            border: iced::Border {
                color: palette.success.strong.color,
                width: 2.0,
                radius: 8.0.into(),
            },
            ..Default::default()
        }
    }

    /// Style for active call overlay
    pub fn active_call(theme: &Theme) -> container::Style {
        let palette = theme.extended_palette();
        container::Style {
            background: Some(iced::Background::Color(palette.background.weak.color)),
            border: iced::Border {
                color: palette.primary.base.color,
                width: 2.0,
                radius: 12.0.into(),
            },
            ..Default::default()
        }
    }

    /// Style for outgoing call notification
    pub fn outgoing_call(theme: &Theme) -> container::Style {
        let palette = theme.extended_palette();
        container::Style {
            background: Some(iced::Background::Color(palette.primary.weak.color)),
            border: iced::Border {
                color: palette.primary.strong.color,
                width: 2.0,
                radius: 8.0.into(),
            },
            ..Default::default()
        }
    }

    /// Style for danger/destructive buttons
    pub fn button_danger(theme: &Theme, status: button::Status) -> button::Style {
        let palette = theme.extended_palette();
        match status {
            button::Status::Active | button::Status::Pressed => button::Style {
                background: Some(iced::Background::Color(palette.danger.base.color)),
                text_color: palette.danger.base.text,
                border: iced::Border {
                    radius: 4.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            },
            button::Status::Hovered => button::Style {
                background: Some(iced::Background::Color(palette.danger.strong.color)),
                text_color: palette.danger.strong.text,
                border: iced::Border {
                    radius: 4.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            },
            button::Status::Disabled => button::Style {
                background: Some(iced::Background::Color(palette.background.strong.color)),
                text_color: palette.background.strong.text,
                border: iced::Border {
                    radius: 4.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            },
        }
    }

    /// Style for success/accept buttons
    pub fn button_success(theme: &Theme, status: button::Status) -> button::Style {
        let palette = theme.extended_palette();
        match status {
            button::Status::Active | button::Status::Pressed => button::Style {
                background: Some(iced::Background::Color(palette.success.base.color)),
                text_color: palette.success.base.text,
                border: iced::Border {
                    radius: 4.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            },
            button::Status::Hovered => button::Style {
                background: Some(iced::Background::Color(palette.success.strong.color)),
                text_color: palette.success.strong.text,
                border: iced::Border {
                    radius: 4.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            },
            button::Status::Disabled => button::Style {
                background: Some(iced::Background::Color(palette.background.strong.color)),
                text_color: palette.background.strong.text,
                border: iced::Border {
                    radius: 4.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            },
        }
    }

    /// Style for icon buttons (transparent background)
    pub fn button_icon(theme: &Theme, status: button::Status) -> button::Style {
        let palette = theme.extended_palette();
        match status {
            button::Status::Active => button::Style {
                background: Some(iced::Background::Color(Color::TRANSPARENT)),
                text_color: palette.background.base.text,
                border: iced::Border {
                    radius: 4.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            },
            button::Status::Hovered => button::Style {
                background: Some(iced::Background::Color(palette.background.weak.color)),
                text_color: palette.background.base.text,
                border: iced::Border {
                    radius: 4.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            },
            button::Status::Pressed => button::Style {
                background: Some(iced::Background::Color(palette.background.strong.color)),
                text_color: palette.background.base.text,
                border: iced::Border {
                    radius: 4.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            },
            button::Status::Disabled => button::Style {
                background: Some(iced::Background::Color(Color::TRANSPARENT)),
                text_color: palette.background.strong.text,
                border: iced::Border {
                    radius: 4.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            },
        }
    }
}

/// Helper functions to get colors from theme
pub mod colors {
    use super::*;

    pub fn text_primary(theme: &Theme) -> Color {
        let palette = theme.extended_palette();
        palette.background.base.text
    }

    pub fn text_secondary(theme: &Theme) -> Color {
        let palette = theme.extended_palette();
        // Use a color with better contrast - not the weak text
        let mut color = palette.background.base.text;
        color.a = 0.7; // Slightly faded but still readable
        color
    }

    pub fn text_error(theme: &Theme) -> Color {
        theme.extended_palette().danger.strong.color
    }

    pub fn text_success(theme: &Theme) -> Color {
        theme.extended_palette().success.strong.color
    }

    pub fn background_base(theme: &Theme) -> Color {
        theme.extended_palette().background.base.color
    }

    pub fn background_weak(theme: &Theme) -> Color {
        theme.extended_palette().background.weak.color
    }

    pub fn background_strong(theme: &Theme) -> Color {
        theme.extended_palette().background.strong.color
    }

    pub fn primary(theme: &Theme) -> Color {
        theme.extended_palette().primary.base.color
    }

    pub fn primary_weak(theme: &Theme) -> Color {
        theme.extended_palette().primary.weak.color
    }

    pub fn primary_strong(theme: &Theme) -> Color {
        theme.extended_palette().primary.strong.color
    }

    pub fn text_muted(theme: &Theme) -> Color {
        let palette = theme.extended_palette();
        let mut color = palette.background.base.text;
        color.a = 0.5; // More muted than secondary
        color
    }

    pub fn success_bg(theme: &Theme) -> Color {
        theme.extended_palette().success.weak.color
    }

    pub fn success_border(theme: &Theme) -> Color {
        theme.extended_palette().success.base.color
    }

    pub fn divider(theme: &Theme) -> Color {
        theme.extended_palette().background.strong.color
    }

    /// Message bubble colors - incoming message
    pub fn message_incoming_bg(theme: &Theme) -> Color {
        theme.extended_palette().success.weak.color
    }

    pub fn message_incoming_border(theme: &Theme) -> Color {
        theme.extended_palette().success.base.color
    }

    /// Message bubble colors - outgoing delivered
    pub fn message_outgoing_bg(theme: &Theme) -> Color {
        theme.extended_palette().primary.weak.color
    }

    pub fn message_outgoing_border(theme: &Theme) -> Color {
        theme.extended_palette().primary.base.color
    }

    /// Message bubble colors - outgoing pending
    pub fn message_pending_bg(theme: &Theme) -> Color {
        let palette = theme.extended_palette();
        let mut color = palette.background.strong.color;
        color.a = 0.8;
        color
    }

    pub fn message_pending_border(theme: &Theme) -> Color {
        theme.extended_palette().background.strong.color
    }
}
