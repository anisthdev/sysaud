use crate::config::sounds::SoundsConfig;
use crate::event::handler::EventHandler;
use crate::event::Event;
use std::path::PathBuf;
use std::sync::Arc;

static AUDIO_EXTENSIONS: &[&str] = &["oga", ".ogg", ".wav", ".flac", ".mp3"];

pub struct SoundHandler {
    config: Arc<crate::config::Config>,
    sounds_config: SoundsConfig,
    enabled: bool,
}

impl SoundHandler {
    pub fn new(
        config: Arc<crate::config::Config>,
        sounds_config: SoundsConfig,
        enabled: bool,
    ) -> Self {
        Self {
            config,
            sounds_config,
            enabled,
        }
    }
}

impl EventHandler for SoundHandler {
    fn handle(&self, event: &Event) -> anyhow::Result<()> {
        if !self.enabled {
            return Ok(());
        }

        let override_config = self.config.events.get(&event.id.as_str());

        if let Some(ov) = override_config {
            if !ov.enabled {
                return Ok(());
            }
        }

        let sound_id = override_config
            .and_then(|ov| ov.sound.as_ref())
            .and_then(|s| if s.is_empty() { None } else { Some(s.as_str()) })
            .or_else(|| self.sounds_config.get(&event.id.as_str()));

        let Some(sound_id) = sound_id else {
            return Ok(());
        };
        if sound_id == "none" {
            return Ok(());
        }

        let is_path = sound_id.contains('/') || sound_id.starts_with('~');
        if is_path {
            play_file(PathBuf::from(sound_id));
        } else {
            play_theme_sound(
                sound_id,
                &self.config.sound.theme,
                &self.config.sound.custom_dir,
            );
        }
        Ok(())
    }
}

fn play_file(path: PathBuf) {
    let path = expand_user(path);
    if !path.exists() {
        tracing::warn!("Sound: file not found: {}", path.display());
        return;
    }

    for player in &["pw-play", "paplay", "aplay"] {
        if which::which(player).is_ok() {
            let result = std::process::Command::new(player)
                .arg(&path)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();

            match result {
                Ok(mut child) => {
                    tracing::debug!("Sound: playing file {} via {}", path.display(), player);
                    match child.wait() {
                        Ok(exit) => {
                            tracing::debug!("Sound: {} exited with {:?}", player, exit.code());
                        }
                        Err(e) => {
                            tracing::warn!("Sound: failed to wait for {}: {}", player, e);
                        }
                    }
                    return;
                }
                Err(e) => {
                    tracing::debug!("Sound: {} failed for {}: {}", player, path.display(), e);
                }
            }
        }
    }
    tracing::warn!("Sound: no audio player found for {}", path.display());
}

fn play_theme_sound(sound_id: &str, theme: &str, custom_dir: &Option<PathBuf>) {
    if let Some(custom_dir) = custom_dir {
        for ext in AUDIO_EXTENSIONS {
            let candidate = custom_dir.join(format!("{sound_id}{ext}"));
            if candidate.exists() {
                play_file(candidate);
                return;
            }
        }
    }

    for theme_dir in theme_dirs(theme) {
        for subdir in &["stereo", ""] {
            let base = if subdir.is_empty() {
                theme_dir.clone()
            } else {
                theme_dir.join(subdir)
            };
            for ext in AUDIO_EXTENSIONS {
                let candidate = base.join(format!("{sound_id}{ext}"));
                if candidate.exists() {
                    play_file(candidate);
                    return;
                }
            }
        }
    }

    tracing::debug!(
        "Sound: no file found for event '{}' in theme '{}'",
        sound_id,
        theme
    );
}

fn expand_user(path: PathBuf) -> PathBuf {
    if path.to_string_lossy().starts_with("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(path.strip_prefix("~/").unwrap_or(&path));
        }
    }
    path
}

fn theme_dirs(theme: &str) -> Vec<PathBuf> {
    let data_home = dirs::data_dir().unwrap_or_else(|| PathBuf::from("~/.local/share"));
    let data_dirs = std::env::var("XDG_DATA_DIRS")
        .unwrap_or_else(|_| "/usr/local/share:/usr/share".to_string());

    let mut dirs = vec![data_home.join("sounds")];
    for p in data_dirs.split(':') {
        if !p.is_empty() {
            dirs.push(PathBuf::from(p).join("sounds"));
        }
    }

    dirs.into_iter().map(|d| d.join(theme)).collect()
}
