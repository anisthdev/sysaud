use crate::config::notifications::NotificationsConfig;
use crate::config::SharedConfig;
use crate::event::Event;
use crate::event::handler::EventHandler;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use once_cell::sync::Lazy;
use zbus::Connection;
use zvariant::Value;

const NOTIFY_BUS: &str = "org.freedesktop.Notifications";
const NOTIFY_PATH: &str = "/org/freedesktop/Notifications";
const NOTIFY_IFACE: &str = "org.freedesktop.Notifications";

const EVENT_APP_NAMES: &[(&str, &str)] = &[
    ("display.brightness-changed", "sysaud-brightness-notification"),
];

const USB_TYPE_ICONS: &[(&str, &str)] = &[
    ("keyboard", "input-keyboard"),
    ("mouse", "input-mouse"),
    ("controller", "input-gaming"),
    ("phone", "phone"),
    ("storage", "drive-removable-media-usb"),
    ("device", "device_usb"),
];

static CURRENT_ICON_THEME: Lazy<Option<String>> = Lazy::new(|| {
    std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "icon-theme"])
        .output()
        .ok()
        .and_then(|out| {
            let theme: String = String::from_utf8_lossy(&out.stdout).into();
            let theme = theme.trim().trim_matches('\'');
            if theme.is_empty() || theme == "\"" {
                None
            } else {
                Some(theme.to_string())
            }
        })
});

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
    }
    
    result.trim().to_string()
}

fn resolve_icon_path(icon_name: &str) -> String {
    if icon_name.is_empty() {
        return String::new();
    }

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

    if let Some(theme) = CURRENT_ICON_THEME.as_ref() {
        for size in &sizes {
            for subdir in &subdirs {
                for ext in &exts {
                    let candidate = PathBuf::from("/usr/share/icons")
                        .join(theme)
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
                    .join(theme)
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

pub fn send_notification(
    summary: &str,
    body: &str,
    icon: &str,
    timeout_ms: i32,
    event: &Event,
    conn: Arc<Connection>,
) {
    let conn = conn;

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
    
    let summary = expand_template(summary, &detail);
    let body = expand_template(body, &detail);
    let icon = expand_template(icon, &detail);
    let event_id = event.id.as_str();

    let mut hints: HashMap<String, Value<'_>> = HashMap::new();
    
    if !icon.is_empty() {
        let icon_path = resolve_icon_path(&icon);
        hints.insert(String::from("image-path"), Value::new(icon_path));
    }
    
    if event_id == "display.brightness-changed" {
        if let Some(level_str) = event.detail.get("level") {
            if let Ok(level) = level_str.parse::<i32>() {
                hints.insert(String::from("value"), Value::new(level));
                hints.insert(String::from("x-canonical-private-synchronous"), Value::new("brightness"));
            }
        }
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
