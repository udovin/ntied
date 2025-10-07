#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use iced::{Application, Settings};
use ntied::ui::ChatApp;
use tracing_subscriber::prelude::*;

fn main() -> iced::Result {
    // Initialize tracing (optional, controlled via RUST_LOG)
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "ntied=info,iced=warn,ntied_transport=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();
    ChatApp::run(Settings::default())
}
