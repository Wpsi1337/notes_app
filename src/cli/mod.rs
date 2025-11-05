use std::env;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use once_cell::sync::OnceCell;
use tracing_subscriber::{fmt, EnvFilter};

use crate::app::App;
use crate::config::ConfigLoader;
use crate::storage;

pub mod commands;

use self::commands::{NewArgs, SearchArgs, TagArgs};

#[derive(Parser, Debug)]
#[command(
    name = "notetui",
    version,
    about = "Ultra-fast terminal notes application"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,

    /// Override the config file location (takes precedence over NOTETUI_CONFIG)
    #[arg(long)]
    pub config: Option<PathBuf>,

    /// Override the data directory (takes precedence over NOTETUI_DATA)
    #[arg(long)]
    pub data_dir: Option<PathBuf>,

    /// Minimum log level (trace, debug, info, warn, error)
    #[arg(long, default_value = "info")]
    pub log_level: String,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Launch the interactive TUI (default)
    Tui,
    /// Create a new note from the command line
    New(NewArgs),
    /// Run a non-interactive search and print matching note titles
    Search(SearchArgs),
    /// Manage note tags from the CLI
    Tag(TagArgs),
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();

    if let Some(path) = &cli.config {
        env::set_var("NOTETUI_CONFIG", path);
    }
    if let Some(path) = &cli.data_dir {
        env::set_var("NOTETUI_DATA", path);
    }

    let loader = ConfigLoader::discover()?;
    loader.paths().ensure_directories()?;
    let paths = loader.paths().clone();
    init_tracing(&cli.log_level)
        .with_context(|| format!("initialising logging at level {}", cli.log_level))?;
    let config = loader.load_or_init()?;
    let storage = storage::init(&paths, &config.storage)?;

    let config = Arc::new(config);
    let command = cli.command.unwrap_or(Commands::Tui);
    match command {
        Commands::Tui => {
            let mut app = App::new(config.clone(), storage.clone(), paths.clone())?;
            commands::run_tui(&mut app)
        }
        Commands::New(args) => commands::new_note(config.clone(), storage.clone(), args),
        Commands::Search(args) => commands::search_notes(config.clone(), storage.clone(), args),
        Commands::Tag(args) => commands::handle_tag_command(config, storage, args),
    }
}

fn init_tracing(level: &str) -> Result<()> {
    static INIT: OnceCell<()> = OnceCell::new();
    INIT.get_or_try_init(|| {
        let env_filter = EnvFilter::try_new(level).unwrap_or_else(|_| EnvFilter::new("info"));
        fmt()
            .with_env_filter(env_filter)
            .with_writer(std::io::stderr)
            .init();
        Ok(())
    })
    .map(|_| ())
}
