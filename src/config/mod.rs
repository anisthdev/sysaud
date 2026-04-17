pub mod debounce;
pub mod notifications;
pub mod sounds;

use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::broadcast;

pub use debounce::DebounceConfig;

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub daemon: DaemonConfig,
    #[serde(default)]
    pub sound: SoundConfig,
    #[serde(default)]
    pub notify: NotifyConfig,
    #[serde(default)]
    pub debounce: DebounceConfig,
    #[serde(default)]
    pub events: HashMap<String, EventOverride>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DaemonConfig {
    #[serde(default = "default_lock_app")]
    pub lock_app: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub log_file: Option<PathBuf>,
}

fn default_lock_app() -> String {
    "hyprlock".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct SoundConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(default)]
    pub custom_dir: Option<PathBuf>,
}

fn default_true() -> bool {
    true
}

fn default_theme() -> String {
    "freedesktop".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct NotifyConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_timeout")]
    pub timeout_ms: i32,
    #[serde(default)]
    #[allow(dead_code)]
    pub icon_theme: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub custom_icon_dir: Option<PathBuf>,
}

fn default_timeout() -> i32 {
    4000
}

#[derive(Debug, Clone, Deserialize)]
pub struct EventOverride {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub sound: Option<String>,
    #[serde(default)]
    pub notify: Option<bool>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub script: Option<String>,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            lock_app: default_lock_app(),
            log_file: None,
        }
    }
}

impl Default for SoundConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            theme: default_theme(),
            custom_dir: None,
        }
    }
}

impl Default for NotifyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            timeout_ms: default_timeout(),
            icon_theme: String::new(),
            custom_icon_dir: None,
        }
    }
}

impl Default for EventOverride {
    fn default() -> Self {
        Self {
            enabled: true,
            sound: None,
            notify: None,
            icon: None,
            script: None,
        }
    }
}

pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("~/.config"))
        .join("sysaud")
        .join("config.yaml")
}

pub fn load(path: Option<PathBuf>) -> anyhow::Result<Config> {
    let config_path = path.unwrap_or_else(config_path);

    if !config_path.exists() {
        tracing::info!(
            "No config file found at {} — using defaults",
            config_path.display()
        );
        return Ok(Config::default());
    }

    let raw = std::fs::read_to_string(&config_path)?;
    let config: Config = serde_yaml::from_str(&raw)?;
    tracing::info!("Loaded config from {}", config_path.display());
    Ok(config)
}

#[derive(Clone)]
pub struct SharedConfig {
    config: Arc<std::sync::RwLock<Config>>,
    reload_tx: broadcast::Sender<Arc<Config>>,
}

impl SharedConfig {
    pub fn new(config: Config) -> Self {
        let (reload_tx, _) = broadcast::channel(1);
        Self {
            config: Arc::new(std::sync::RwLock::new(config)),
            reload_tx,
        }
    }

    pub fn get_config(&self) -> Arc<Config> {
        Arc::new(self.config.read().unwrap().clone())
    }

    pub fn reload(&self, new_config: Config) {
        let new_shared = Arc::new(new_config);
        let _ = self.reload_tx.send(new_shared.clone());
    }
}
