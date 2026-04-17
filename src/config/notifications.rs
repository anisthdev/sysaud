use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct NotificationSpec {
    pub summary: String,
    pub body: String,
    pub icon: String,
}

#[derive(Debug, Clone)]
pub struct NotificationsConfig {
    pub defaults: HashMap<String, NotificationSpec>,
}

impl Default for NotificationsConfig {
    fn default() -> Self {
        Self::load().unwrap_or_else(|e| {
            tracing::warn!(
                "Failed to load notifications.toml, using empty defaults: {}",
                e
            );
            Self {
                defaults: HashMap::new(),
            }
        })
    }
}

impl NotificationsConfig {
    pub fn load() -> anyhow::Result<Self> {
        let path = Self::path()?;
        tracing::debug!("notifications path: {:?}", path);
        if !path.exists() {
            tracing::debug!("notifications file does not exist at {:?}", path);
            return Ok(Self {
                defaults: HashMap::new(),
            });
        }

        let raw = std::fs::read_to_string(&path)?;
        let data: toml::Value = toml::from_str(&raw)?;

        let mut defaults = HashMap::new();
        if let Some(toml::Value::Table(defaults_table)) = data.get("defaults") {
            for (key, value) in defaults_table {
                if let Some(spec) = value.as_table() {
                    let summary = spec
                        .get("summary")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let body = spec
                        .get("body")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let icon = spec
                        .get("icon")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();

                    defaults.insert(
                        key.clone(),
                        NotificationSpec {
                            summary,
                            body,
                            icon,
                        },
                    );
                }
            }
        }

        tracing::debug!(
            "Loaded {} notification specs from {}",
            defaults.len(),
            path.display()
        );
        Ok(Self { defaults })
    }

    fn path() -> anyhow::Result<PathBuf> {
        let exe_path = std::env::current_exe()?;
        let base_dir = exe_path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Executable has no parent directory"))?
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Executable has no grandparent directory"))?;

        let installed_path = base_dir.join("share/sysaud/notifications.toml");
        if installed_path.exists() {
            return Ok(installed_path);
        }

        let repo_root = base_dir
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Executable has no great-grandparent directory"))?;
        let dev_path = repo_root.join("data/notifications.toml");
        if dev_path.exists() {
            tracing::debug!("Using dev notifications.toml at {}", dev_path.display());
            return Ok(dev_path);
        }

        Ok(installed_path)
    }

    pub fn get(&self, event_id: &str) -> Option<&NotificationSpec> {
        self.defaults.get(event_id)
    }
}
