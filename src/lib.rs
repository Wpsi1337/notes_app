pub mod app;
pub mod cli;
pub mod config;
pub mod highlight;
pub mod journaling;
pub mod search;
pub mod storage;
pub mod ui;

pub use config::{AppConfig, ConfigLoader, ConfigPaths};
