use crate::event::handler::EventHandler;
use crate::event::Event;
use chrono::Utc;
use std::io::Write;

pub struct LoggerHandler {
    config: crate::config::SharedConfig,
}

impl LoggerHandler {
    pub fn new(config: crate::config::SharedConfig) -> Self {
        Self { config }
    }
}

impl EventHandler for LoggerHandler {
    fn handle(&self, event: &Event) -> anyhow::Result<()> {
        let config = self.config.snapshot();
        let log_file = match &config.daemon.log_file {
            Some(path) => path,
            None => return Ok(()),
        };

        let log_entry = serde_json::json!({
            "timestamp": Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            "event": event.id.as_str(),
            "category": event.id.category.clone(),
            "name": event.id.name.clone(),
            "detail": event.detail.clone(),
        });

        let json = match serde_json::to_string(&log_entry) {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!("Failed to serialize log entry: {}", e);
                return Ok(());
            }
        };

        let line = json + "\n";

        let mut file = match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_file)
        {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!("Failed to open log file {}: {}", log_file.display(), e);
                return Ok(());
            }
        };

        if let Err(e) = file.write_all(line.as_bytes()) {
            tracing::warn!("Failed to write to log file: {}", e);
        }

        Ok(())
    }
}
