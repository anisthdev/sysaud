use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Deserialize)]
pub struct DebounceConfig {
    #[serde(default = "default_global_window")]
    pub global: f64,
    #[serde(default)]
    pub specific: HashMap<String, f64>,
}

fn default_global_window() -> f64 {
    0.1
}

impl Default for DebounceConfig {
    fn default() -> Self {
        let mut specific = HashMap::new();
        specific.insert("display.brightness-changed".to_string(), 0.0);
        specific.insert("network.network-disconnected".to_string(), 2.0);
        specific.insert("network.network-connected".to_string(), 2.0);
        specific.insert("network.wifi-connected".to_string(), 2.0);
        specific.insert("power.battery-level".to_string(), 5.0);

        Self {
            global: 0.1,
            specific,
        }
    }
}

impl DebounceConfig {
    pub fn window_for(&self, event_id: &crate::event::EventId) -> f64 {
        let event_str = event_id.as_str();
        self.specific
            .get(&event_str)
            .copied()
            .unwrap_or(self.global)
    }
}
