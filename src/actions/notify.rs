use crate::config::notifications::NotificationsConfig;
use crate::config::SharedConfig;
use crate::event::Event;
use crate::event::handler::EventHandler;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use zbus::Connection;
use zvariant::Value;

const NOTIFY_BUS: &str = "org.freedesktop.Notifications";
const NOTIFY_PATH: &str = "/org/freedesktop/Notifications";
const NOTIFY_IFACE: &str = "org.freedesktop.Notifications";

const EVENT_APP_NAMES: &[(&str, &str)] = &[
    ("display.brightness-changed", "volume-notification"),
    ("audio.volume-changed", "volume-notification"),
    ("audio.mute-changed", "volume-notification"),
    ("audio.default-sink-changed", "sysaud-audio"),
    ("audio.default-source-changed", "sysaud-audio"),
];

const USB_TYPE_ICONS: &[(&str, &str)] = &[
    ("keyboard", "input-keyboard"),
    ("mouse", "input-mouse"),
    ("controller", "input-gaming"),
    ("phone", "phone"),
    ("storage", "drive-removable-media-usb"),
    ("device", "drive-removable-media"),
];

const ICON_ALIASES: &[(&str, &str)] = &[
    ("battery-discharging", "battery-caution"),
    ("bluetooth-disconnected", "bluetooth-active"),
    ("device_usb", "drive-removable-media"),
];

pub struct NotifyHandler {
    config: SharedConfig,
    notifications_config: NotificationsConfig,
    enabled: bool,
    session_bus: Arc<Connection>,
}

impl NotifyHandler {
    pub fn new(
        config: SharedConfig,
        notifications_config: NotificationsConfig,
        enabled: bool,
        session_bus: Connection,
    ) -> Self {
        Self {
            config,
            notifications_config,
            enabled,
            session_bus: Arc::new(session_bus),
        }
    }
}

impl EventHandler for NotifyHandler {
    fn handle(&self, event: &Event) -> anyhow::Result<()> {
        if !self.enabled {
            return Ok(());
        }

        let config = self.config.snapshot();

        if !config.notify.enabled {
            return Ok(());
        }

        let override_config = config.events.get(&event.id.as_str());

        if let Some(ov) = override_config {
            if !ov.enabled {
                return Ok(());
            }
        }

        let notify_enabled = override_config.and_then(|ov| ov.notify).unwrap_or(true);

        if !notify_enabled {
            return Ok(());
        }

        if let Some(spec) = self.notifications_config.get(&event.id.as_str()) {
            let icon = override_config
                .and_then(|ov| ov.icon.clone())
                .unwrap_or_else(|| spec.icon.clone());

            send_notification(
                &spec.summary,
                &spec.body,
                &icon,
                config.notify.timeout_ms,
                event,
                self.session_bus.clone(),
            );
        }
        Ok(())
    }
}

fn get_app_name(event_id: &str, category: &str) -> String {
    EVENT_APP_NAMES
        .iter()
        .find(|(id, _)| id == &event_id)
        .map(|(_, name)| name.to_string())
        .unwrap_or_else(|| format!("sysaud-{category}"))
}

fn expand_template(template: &str, detail: &HashMap<String, String>) -> String {
    let mut result = template.to_string();

    for (key, value) in detail {
        let placeholder = format!("{{{key}}}");
        result = result.replace(&placeholder, value);

        let float_placeholder = format!("{{{key}:.0f}}");
        if result.contains(&float_placeholder) {
            let formatted = value
                .parse::<f64>()
                .map(|number| format!("{number:.0}"))
                .unwrap_or_else(|_| value.clone());
            result = result.replace(&float_placeholder, &formatted);
        }
    }

    result.trim().to_string()
}

fn resolve_icon_path(icon_name: &str) -> String {
    if icon_name.is_empty() {
        return String::new();
    }

    let icon_name = ICON_ALIASES
        .iter()
        .find(|(alias, _)| *alias == icon_name)
        .map(|(_, canonical)| *canonical)
        .unwrap_or(icon_name);

    let icon_path = Path::new(icon_name);
    if icon_path.is_absolute() {
        return icon_name.to_string();
    }

    if let Some(stripped) = icon_name.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            let resolved = PathBuf::from(home).join(stripped);
            if resolved.exists() {
                return resolved.to_string_lossy().to_string();
            }
        }
    }

    let sizes = ["48x48", "32x32", "24x24", "16x16", "scalable"];
    let subdirs = ["apps", "devices", "mimetypes", "places", "status", "legacy", "panel", "actions", "emotes", "animations"];
    let exts = ["svg", "png"];

    if let Some(theme) = current_icon_theme() {
        for size in &sizes {
            for subdir in &subdirs {
                for ext in &exts {
                    let candidate = PathBuf::from("/usr/share/icons")
                        .join(&theme)
                        .join(size)
                        .join(subdir)
                        .join(format!("{icon_name}.{ext}"));
                    if candidate.exists() {
                        return candidate.to_string_lossy().to_string();
                    }
                }
            }
            for ext in &exts {
                let candidate = PathBuf::from("/usr/share/icons")
                    .join(&theme)
                    .join(size)
                    .join(format!("{icon_name}.{ext}"));
                if candidate.exists() {
                    return candidate.to_string_lossy().to_string();
                }
            }
        }
    }

    let icon_bases = [
        "/usr/share/icons/hicolor",
        "/usr/share/icons/Adwaita",
        "/usr/share/icons/AdwaitaLegacy",
        "/usr/share/icons/gnome",
        "/usr/share/icons/Papirus",
        "/usr/share/icons/Papirus-Light",
        "/usr/share/icons/Win11",
        "/usr/share/icons/Win11-dark",
        "/usr/share/icons/HighContrast",
    ];

    for base in &icon_bases {
        for size in &sizes {
            for subdir in &subdirs {
                for ext in &exts {
                    let candidate = PathBuf::from(base).join(size).join(subdir).join(format!("{icon_name}.{ext}"));
                    if candidate.exists() {
                        return candidate.to_string_lossy().to_string();
                    }
                }
            }
            for ext in &exts {
                let candidate = PathBuf::from(base).join(size).join(format!("{icon_name}.{ext}"));
                if candidate.exists() {
                    return candidate.to_string_lossy().to_string();
                }
            }
        }
    }

    icon_name.to_string()
}

fn current_icon_theme() -> Option<String> {
    std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "icon-theme"])
        .output()
        .ok()
        .and_then(|out| parse_icon_theme_output(&String::from_utf8_lossy(&out.stdout)))
}

fn parse_icon_theme_output(output: &str) -> Option<String> {
    let theme = output.trim().trim_matches('\'');
    if theme.is_empty() || theme == "\"" {
        None
    } else {
        Some(theme.to_string())
    }
}

pub fn send_notification(
    summary: &str,
    body: &str,
    icon: &str,
    timeout_ms: i32,
    event: &Event,
    conn: Arc<Connection>,
) {
    let app_name = get_app_name(&event.id.as_str(), &event.id.category);
    
    let mut detail = event.detail.clone();
    if event.id.category == "usb" {
        if let Some(device_type) = detail.get("type") {
            let type_icon = USB_TYPE_ICONS
                .iter()
                .find(|(t, _)| *t == device_type)
                .map(|(_, icon)| icon)
                .unwrap_or(&"drive-removable-media");
            detail.insert("type_icon".to_string(), type_icon.to_string());
        }
    }

    if let Some(volume_str) = detail.get("volume") {
        if let Ok(volume) = volume_str.parse::<i32>() {
            let volume_icon = if volume < 30 {
                "audio-volume-low"
            } else if volume < 70 {
                "audio-volume-medium"
            } else {
                "audio-volume-high"
            };
            detail.insert("volume_icon".to_string(), volume_icon.to_string());
        }
    }

    if let Some(muted_str) = detail.get("muted") {
        let muted = muted_str == "true";
        detail.insert(
            "muted_str".to_string(),
            if muted { "Muted" } else { "Unmuted" }.to_string(),
        );
        detail.insert(
            "muted_icon".to_string(),
            if muted { "audio-volume-muted" } else { "audio-volume-high" }.to_string(),
        );
    }
    
    let summary = expand_template(summary, &detail);
    let body = expand_template(body, &detail);
    let icon = expand_template(icon, &detail);
    let mut hints: HashMap<String, Value<'_>> = HashMap::new();
    let event_id = event.id.as_str();
    
    if !icon.is_empty() {
        let icon_path = resolve_icon_path(&icon);
        if Path::new(&icon_path).is_absolute() {
            hints.insert(String::from("image-path"), Value::new(icon_path));
        }
    }
    
    if event_id == "display.brightness-changed" {
        if let Some(level_str) = event.detail.get("level") {
            if let Ok(level) = level_str.parse::<i32>() {
                hints.insert(String::from("value"), Value::new(level));
                hints.insert(String::from("x-canonical-private-synchronous"), Value::new("brightness"));
                hints.insert(String::from("urgency"), Value::new(0u8));
            }
        }
    } else if event_id == "audio.volume-changed" {
        if let Some(level_str) = event.detail.get("volume") {
            if let Ok(level) = level_str.parse::<i32>() {
                hints.insert(String::from("value"), Value::new(level));
            }
        }
        hints.insert(String::from("x-canonical-private-synchronous"), Value::new("volume_notif"));
        hints.insert(String::from("urgency"), Value::new(0u8));
    }

    tokio::spawn(async move {
        let proxy = match zbus::Proxy::new(
            &conn,
            NOTIFY_BUS,
            NOTIFY_PATH,
            NOTIFY_IFACE,
        ).await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("Notify: failed to create proxy: {}", e);
                return;
            }
        };

        let reply = proxy.call_method(
            "Notify",
            &(app_name, 0u32, icon, summary, body, Vec::<String>::new(), hints, timeout_ms),
        ).await;

        match reply {
            Ok(response) => {
                let id: u32 = response.body().deserialize().unwrap_or(0);
                tracing::debug!("Notify: sent notification (id: {})", id);
            }
            Err(e) => {
                tracing::warn!("Notify: failed to send notification: {}", e);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{expand_template, get_app_name, parse_icon_theme_output};
    use std::collections::HashMap;

    #[test]
    fn expands_plain_placeholders() {
        let detail = HashMap::from([("direction".to_string(), "charging".to_string())]);
        assert_eq!(expand_template("{direction}", &detail), "charging");
    }

    #[test]
    fn expands_numeric_placeholders_with_float_format() {
        let detail = HashMap::from([("percentage".to_string(), "39.6".to_string())]);
        assert_eq!(expand_template("Battery {percentage:.0f}%", &detail), "Battery 40%");
    }

    #[test]
    fn parses_gsettings_icon_theme_output() {
        assert_eq!(
            parse_icon_theme_output("'Papirus'\n").as_deref(),
            Some("Papirus")
        );
        assert_eq!(parse_icon_theme_output("''\n"), None);
    }

    #[test]
    fn brightness_uses_volume_notification_app_name() {
        assert_eq!(
            get_app_name("display.brightness-changed", "display"),
            "volume-notification"
        );
    }
}
