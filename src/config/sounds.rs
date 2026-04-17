use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct SoundsConfig {
    pub defaults: HashMap<String, String>,
}

impl Default for SoundsConfig {
    fn default() -> Self {
        Self::load().unwrap_or_else(|e| {
            tracing::warn!("Failed to load sounds.toml, using empty defaults: {}", e);
            Self {
                defaults: HashMap::new(),
            }
        })
    }
}

impl SoundsConfig {
    pub fn load() -> anyhow::Result<Self> {
        let path = Self::path()?;
        if !path.exists() {
            return Ok(Self {
                defaults: HashMap::new(),
            });
        }

        let raw = std::fs::read_to_string(&path)?;
        let data: toml::Value = toml::from_str(&raw)?;

        let mut defaults = HashMap::new();
        if let Some(toml::Value::Table(defaults_table)) = data.get("defaults") {
            for (key, value) in defaults_table {
                if let Some(sound) = value.as_str() {
                    defaults.insert(key.clone(), sound.to_string());
                }
            }
        }

        tracing::debug!(
            "Loaded {} sound mappings from {}",
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

        let installed_path = base_dir.join("share/sysaud/sounds.toml");
        if installed_path.exists() {
            return Ok(installed_path);
        }

        let repo_root = base_dir
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Executable has no great-grandparent directory"))?;
        let dev_path = repo_root.join("data/sounds.toml");
        if dev_path.exists() {
            tracing::debug!("Using dev sounds.toml at {}", dev_path.display());
            return Ok(dev_path);
        }

        Ok(installed_path)
    }

    pub fn get(&self, event_id: &str) -> Option<&str> {
        self.defaults.get(event_id).map(|s| s.as_str())
    }
}
