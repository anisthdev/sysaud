use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use super::handler::EventHandler;
use super::Event;
use super::EventId;
use crate::config::DebounceConfig;

pub struct EventRouter {
    handlers: Vec<Box<dyn EventHandler>>,
    debounce: DebounceConfig,
    last_fired: Mutex<HashMap<EventId, Instant>>,
}

impl EventRouter {
    pub fn new(handlers: Vec<Box<dyn EventHandler>>, debounce: DebounceConfig) -> Self {
        Self {
            handlers,
            debounce,
            last_fired: Mutex::new(HashMap::new()),
        }
    }

    pub fn handle(&self, event: &Event) {
        self.receive(event);
    }

    pub fn receive(&self, event: &Event) {
        let now = Instant::now();
        let window = self.debounce.window_for(&event.id);

        let should_process = {
            let mut last_fired = self.last_fired.lock().unwrap();
            let last = last_fired.get(&event.id);

            let should_process = if let Some(last_instant) = last {
                let elapsed = now.duration_since(*last_instant).as_secs_f64();
                elapsed >= window
            } else {
                true
            };

            if should_process {
                last_fired.insert(event.id.clone(), now);
            }

            should_process
        };

        if !should_process {
            tracing::debug!("Debounced {}", event.id);
            return;
        }

        tracing::info!("Event: {}", event.id);

        for handler in &self.handlers {
            if let Err(e) = handler.handle(event) {
                tracing::error!("Handler error for {}: {}", event.id, e);
            }
        }
    }
}
