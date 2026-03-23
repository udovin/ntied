#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use iced::window::{Icon, Settings, icon};
use ntied::ui::ChatApp;
use tracing_subscriber::prelude::*;

const STACK_SIZE: usize = 16 * 1024 * 1024;

struct Executor(tokio::runtime::Runtime);

impl iced::Executor for Executor {
    fn new() -> Result<Self, std::io::Error> {
        tokio::runtime::Builder::new_multi_thread()
            .thread_stack_size(STACK_SIZE)
            .enable_all()
            .build()
            .map(Executor)
    }

    #[allow(clippy::let_underscore_future)]
    fn spawn(&self, future: impl std::future::Future<Output = ()> + Send + 'static) {
        let _ = self.0.spawn(future);
    }

    fn enter<R>(&self, f: impl FnOnce() -> R) -> R {
        let _guard = self.0.enter();
        f()
    }
}

fn main() -> iced::Result {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "ntied=debug,iced=warn,ntied_transport=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();
    iced::application(ChatApp::title, ChatApp::update, ChatApp::view)
        .executor::<Executor>()
        .theme(ChatApp::theme)
        .window(Settings {
            icon: window_icon(),
            ..Default::default()
        })
        .subscription(ChatApp::subscription)
        .run_with(ChatApp::new)
}

fn window_icon() -> Option<Icon> {
    const ICON_DATA: &[u8] = include_bytes!("../assets/ntied-icon.png");
    let image = image::load_from_memory(ICON_DATA).ok()?;
    let rgba = image.into_rgba8();
    let width = rgba.width();
    let height = rgba.height();
    let pixels = rgba.into_raw();
    icon::from_rgba(pixels, width, height).ok()
}
