use std::collections::HashMap;
use std::time::Instant;

use super::handler::EventHandler;
use super::Event;
use super::EventId;
use crate::config::DebounceConfig;
use parking_lot::Mutex;

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

        let should_process = self.should_process(event, now, window);

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

    fn should_process(&self, event: &Event, now: Instant, window: f64) -> bool {
        let mut last_fired = self.last_fired.lock();
        let should_process = match last_fired.get(&event.id) {
            Some(last_instant) => now.duration_since(*last_instant).as_secs_f64() >= window,
            None => true,
        };

        if should_process {
            last_fired.insert(event.id.clone(), now);
        }

        should_process
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct CountingHandler {
        hits: Arc<AtomicUsize>,
    }

    impl EventHandler for CountingHandler {
        fn handle(&self, _event: &Event) -> anyhow::Result<()> {
            self.hits.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn debounces_repeated_events() {
        let hits = Arc::new(AtomicUsize::new(0));
        let router = EventRouter::new(
            vec![Box::new(CountingHandler {
                hits: hits.clone(),
            })],
            DebounceConfig {
                global: 60.0,
                specific: HashMap::new(),
            },
        );
        let event = Event::new("power", "ac-plugged", HashMap::new());

        router.receive(&event);
        router.receive(&event);

        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }
}
