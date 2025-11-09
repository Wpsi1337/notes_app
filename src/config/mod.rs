use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use time::Duration;

use crate::config::themes::ThemeRegistry;

pub mod themes;

const APP_DOMAIN: &str = "io";
const APP_ORG: &str = "NotesTui";
const APP_NAME: &str = "notetui";

pub struct ConfigLoader {
    paths: ConfigPaths,
}

impl ConfigLoader {
    pub fn discover() -> Result<Self> {
        let paths = ConfigPaths::discover()?;
        Ok(Self { paths })
    }

    pub fn paths(&self) -> &ConfigPaths {
        &self.paths
    }

    pub fn load_or_init(&self) -> Result<AppConfig> {
        self.paths.ensure_directories()?;
        if !self.paths.config_file.exists() {
            let mut default_cfg = AppConfig::default();
            default_cfg.post_load(&self.paths)?;
            self.write_default_config(&default_cfg)?;
            return Ok(default_cfg);
        }

        self.load()
    }

    pub fn load(&self) -> Result<AppConfig> {
        let raw = fs::read_to_string(&self.paths.config_file)
            .with_context(|| format!("reading config {}", self.paths.config_file.display()))?;
        let mut cfg: AppConfig = toml::from_str(&raw).context("parsing config toml")?;
        cfg.post_load(&self.paths)?;
        Ok(cfg)
    }

    fn write_default_config(&self, cfg: &AppConfig) -> Result<()> {
        let toml = toml::to_string_pretty(cfg).context("serializing default config")?;
        if let Some(parent) = self.paths.config_file.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        let mut file = fs::File::create(&self.paths.config_file)
            .with_context(|| format!("creating config {}", self.paths.config_file.display()))?;
        file.write_all(toml.as_bytes())
            .context("writing default config")?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct ConfigPaths {
    pub config_dir: PathBuf,
    pub config_file: PathBuf,
    pub data_dir: PathBuf,
    pub database_path: PathBuf,
    pub cache_dir: PathBuf,
    pub backup_dir: PathBuf,
    pub log_dir: PathBuf,
    pub state_dir: PathBuf,
}

impl ConfigPaths {
    pub fn discover() -> Result<Self> {
        let override_config = env::var("NOTETUI_CONFIG").ok().map(PathBuf::from);
        let override_data = env::var("NOTETUI_DATA").ok().map(PathBuf::from);

        let project_dirs = ProjectDirs::from(APP_DOMAIN, APP_ORG, APP_NAME)
            .context("resolving XDG project directories")?;

        let config_dir = override_config
            .clone()
            .map(|p| {
                if p.is_dir() {
                    p
                } else {
                    p.parent().map(Path::to_path_buf).unwrap_or(p)
                }
            })
            .unwrap_or_else(|| project_dirs.config_dir().to_path_buf());

        let config_file = override_config
            .filter(|p| p.is_file() || p.extension().is_some())
            .unwrap_or_else(|| config_dir.join("config.toml"));

        let data_root = override_data.unwrap_or_else(|| project_dirs.data_dir().to_path_buf());
        let database_path = data_root.join("notes.db");

        let cache_dir = project_dirs.cache_dir().to_path_buf();
        let state_dir = project_dirs
            .state_dir()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| data_root.join("state"));
        let log_dir = state_dir.join("logs");
        let backup_dir = data_root.join("backups");

        Ok(Self {
            config_dir,
            config_file,
            data_dir: data_root,
            database_path,
            cache_dir,
            backup_dir,
            log_dir,
            state_dir,
        })
    }

    pub fn ensure_directories(&self) -> Result<()> {
        for dir in [
            &self.config_dir,
            &self.data_dir,
            &self.cache_dir,
            &self.backup_dir,
            &self.log_dir,
            &self.state_dir,
        ] {
            fs::create_dir_all(dir)
                .with_context(|| format!("creating application directory {}", dir.display()))?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub theme: ThemeName,
    pub preview_lines: u16,
    pub default_sort: SortSpec,
    pub auto_save: AutoSaveConfig,
    pub keybindings: KeybindingProfile,
    pub storage: StorageOptions,
    pub search: SearchOptions,
    pub retention_days: u32,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            theme: ThemeName::Dark,
            preview_lines: 5,
            default_sort: SortSpec {
                field: SortField::Updated,
                direction: SortDirection::Descending,
            },
            auto_save: AutoSaveConfig::default(),
            keybindings: KeybindingProfile::Vim,
            storage: StorageOptions::default(),
            search: SearchOptions::default(),
            retention_days: 30,
        }
    }
}

impl AppConfig {
    fn post_load(&mut self, paths: &ConfigPaths) -> Result<()> {
        self.storage
            .resolve(paths)
            .context("resolving storage paths")?;
        if !ThemeRegistry::default().contains(&self.theme) {
            tracing::warn!(?self.theme, "unknown theme in config, falling back to Dark");
            self.theme = ThemeName::Dark;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AutoSaveConfig {
    pub debounce_ms: u64,
    pub enabled: bool,
    pub crash_recovery: bool,
    /// Retain crash-recovery snapshots for this many hours (0 = keep indefinitely)
    pub snapshot_retention_hours: u64,
}

impl Default for AutoSaveConfig {
    fn default() -> Self {
        Self {
            debounce_ms: 800,
            enabled: true,
            crash_recovery: true,
            snapshot_retention_hours: 24 * 7,
        }
    }
}

impl AutoSaveConfig {
    pub fn debounce_duration(&self) -> Duration {
        Duration::milliseconds(self.debounce_ms as i64)
    }

    pub fn snapshot_retention(&self) -> Option<Duration> {
        if self.snapshot_retention_hours == 0 {
            None
        } else {
            Some(Duration::hours(self.snapshot_retention_hours as i64))
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageOptions {
    #[serde(skip)]
    pub database_path: PathBuf,
    #[serde(skip)]
    pub backup_dir: PathBuf,
    pub wal_autocheckpoint: u32,
    pub backup_on_exit: bool,
}

impl Default for StorageOptions {
    fn default() -> Self {
        Self {
            database_path: PathBuf::new(),
            backup_dir: PathBuf::new(),
            wal_autocheckpoint: 1000,
            backup_on_exit: true,
        }
    }
}

impl StorageOptions {
    fn resolve(&mut self, paths: &ConfigPaths) -> Result<()> {
        if self.database_path.as_os_str().is_empty() {
            self.database_path = paths.database_path.clone();
        }
        if self.backup_dir.as_os_str().is_empty() {
            self.backup_dir = paths.backup_dir.clone();
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchOptions {
    pub max_results: usize,
    pub regex_default: bool,
    pub fuzzy_threshold: f32,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            max_results: 200,
            regex_default: false,
            fuzzy_threshold: 0.4,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, std::hash::Hash)]
#[serde(rename_all = "kebab-case")]
pub enum ThemeName {
    Dark,
    Light,
    HighContrast,
    Solarized,
}

impl Default for ThemeName {
    fn default() -> Self {
        ThemeName::Dark
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum KeybindingProfile {
    Vim,
    Emacs,
    Custom,
}

impl Default for KeybindingProfile {
    fn default() -> Self {
        KeybindingProfile::Vim
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SortSpec {
    pub field: SortField,
    pub direction: SortDirection,
}

impl Default for SortSpec {
    fn default() -> Self {
        Self {
            field: SortField::Updated,
            direction: SortDirection::Descending,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SortField {
    Updated,
    Created,
    Title,
}

impl Default for SortField {
    fn default() -> Self {
        SortField::Updated
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SortDirection {
    Ascending,
    Descending,
}

impl Default for SortDirection {
    fn default() -> Self {
        SortDirection::Descending
    }
}
