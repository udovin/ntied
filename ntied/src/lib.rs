pub mod audio;
pub mod call;
pub mod chat;
pub mod contact;
pub mod models;
pub mod packet;
pub mod storage;

// Configuration manager (account/profile/server settings backed by storage)
pub mod config;

// UI module is always available.
pub mod ui;

pub const DEFAULT_SERVER: &str = "127.0.0.1:39045";
