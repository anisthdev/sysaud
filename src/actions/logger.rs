use crate::event::handler::EventHandler;
use crate::event::Event;
use chrono::Local;
use std::io::Write;
use std::sync::Arc;

pub struct LoggerHandler {
    config: Arc<crate::config::Config>,
}

impl LoggerHandler {
    pub fn new(config: Arc<crate::config::Config>) -> Self {
        Self { config }
    }
}

impl EventHandler for LoggerHandler {
    fn handle(&self, event: &Event) -> anyhow::Result<()> {
        let log_file = match &self.config.daemon.log_file {
            Some(path) => path,
            None => return Ok(()),
        };

        let log_entry = serde_json::json!({
            "timestamp": Local::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string(),
            "event": event.id.as_str(),
            "category": event.id.category,
            "name": event.id.name,
            "detail": event.detail,
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
