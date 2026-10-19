use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use futures_util::FutureExt;
use tokio::sync::{mpsc, watch};
use tokio::time::{sleep, Duration};
use crate::event::Event;
use crate::detectors::context::DetectorContext;
use crate::detectors::detector::Detector;

mod bluetooth;
mod audio;
pub mod context;
pub mod detector;
mod disk;
mod display;
mod map_state;
mod network;
mod power;
mod state;
mod session;
mod usb;

const MAX_RESTART_ATTEMPTS: u32 = 5;
const INITIAL_BACKOFF_MS: u64 = 1000;
const MAX_BACKOFF_MS: u64 = 60000;
/// How long all detectors together get to stop before stragglers are aborted.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);
/// A run at least this long counts as healthy, so a later failure starts the
/// restart budget and backoff over (e.g. surviving many pipewire restarts).
const HEALTHY_RUN: Duration = Duration::from_secs(60);

#[derive(Clone)]
pub struct DetectorRegistry {
    event_tx: mpsc::Sender<Event>,
    shutdown_tx: watch::Sender<bool>,
    handles: Arc<tokio::sync::Mutex<Vec<(&'static str, tokio::task::JoinHandle<()>)>>>,
}

fn panic_message(panic: &Box<dyn std::any::Any + Send>) -> &str {
    panic
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("<non-string panic payload>")
}

impl DetectorRegistry {
    pub fn new(event_tx: mpsc::Sender<Event>) -> Self {
        let (shutdown_tx, _) = watch::channel(false);
        Self {
            event_tx,
            shutdown_tx,
            handles: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        }
    }

    pub async fn attach_all(
        &self,
        session_bus: zbus::Connection,
        system_bus: zbus::Connection,
        config: crate::config::SharedConfig,
    ) -> anyhow::Result<()> {
        let ctx = DetectorContext {
            event_tx: self.event_tx.clone(),
            config: config.clone(),
            session_bus,
            system_bus,
        };

        self.spawn_with_supervision::<session::SessionDetector>(ctx.clone(), self.shutdown_tx.subscribe()).await;
        self.spawn_with_supervision::<session::SuspendDetector>(ctx.clone(), self.shutdown_tx.subscribe()).await;
        self.spawn_with_supervision::<network::NetworkDetector>(ctx.clone(), self.shutdown_tx.subscribe()).await;
        self.spawn_with_supervision::<network::VpnDetector>(ctx.clone(), self.shutdown_tx.subscribe()).await;
        self.spawn_with_supervision::<power::PowerDetector>(ctx.clone(), self.shutdown_tx.subscribe()).await;
        self.spawn_with_supervision::<power::BatteryDetector>(ctx.clone(), self.shutdown_tx.subscribe()).await;
        self.spawn_with_supervision::<bluetooth::BluetoothDetector>(ctx.clone(), self.shutdown_tx.subscribe()).await;
        self.spawn_with_supervision::<audio::AudioDetector>(ctx.clone(), self.shutdown_tx.subscribe()).await;
        self.spawn_with_supervision::<usb::UsbDetector>(ctx.clone(), self.shutdown_tx.subscribe()).await;
        self.spawn_with_supervision::<disk::DiskDetector>(ctx.clone(), self.shutdown_tx.subscribe()).await;
        self.spawn_with_supervision::<display::DisplayDetector>(ctx.clone(), self.shutdown_tx.subscribe()).await;

        tracing::info!("Detector registry started with all detectors");
        Ok(())
    }

    pub async fn shutdown(&self) {
        tracing::info!("Shutting down all detectors...");
        let _ = self.shutdown_tx.send(true);
        
        let handles: Vec<_> = self.handles.lock().await.drain(..).collect();
        tracing::debug!("DEBUG: Registry shutdown - draining {} handles", handles.len());
        let deadline = tokio::time::Instant::now() + SHUTDOWN_TIMEOUT;
        for (name, mut handle) in handles {
            tracing::debug!("DEBUG: Awaiting detector handle [{}]", name);
            match tokio::time::timeout_at(deadline, &mut handle).await {
                Ok(_) => tracing::debug!("DEBUG: Detector handle [{}] finished", name),
                Err(_) => {
                    tracing::warn!("[{}] Detector did not stop within {:?}, aborting", name, SHUTDOWN_TIMEOUT);
                    handle.abort();
                }
            }
        }
        tracing::info!("All detectors stopped");
    }

    async fn spawn_with_supervision<D: Detector + 'static>(&self, ctx: DetectorContext, mut shutdown: watch::Receiver<bool>) {
        let detector_name = D::new(self.event_tx.clone()).name();
        let handle = tokio::spawn(async move {
            let mut attempts = 0u32;
            let mut backoff_ms = INITIAL_BACKOFF_MS;

            loop {
                let detector = D::new(ctx.event_tx.clone());
                let name = detector.name().to_string();

                tracing::info!("[{}] Starting detector", name);

                let shutdown_rx = shutdown.clone();
                let started_at = tokio::time::Instant::now();
                // A panic inside a detector would otherwise kill this supervisor task
                // and the detector with it; handle it like any other failure.
                let result = AssertUnwindSafe(Arc::new(detector).start(ctx.clone(), shutdown_rx))
                    .catch_unwind()
                    .await
                    .unwrap_or_else(|panic| Err(anyhow::anyhow!("panicked: {}", panic_message(&panic))));

                // Detectors return Ok when their event source ends (D-Bus connection
                // dropped, pactl exited, ...). That is only a graceful stop if we
                // asked for it; otherwise the source went away and must be retried.
                let result = match result {
                    Ok(()) if !*shutdown.borrow() => Err(anyhow::anyhow!("event source ended unexpectedly")),
                    other => other,
                };

                match result {
                    Ok(()) => {
                        tracing::info!("[{}] Detector stopped gracefully", name);
                        tracing::debug!("DEBUG: [{}] Loop exiting", name);
                        break;
                    }
                    Err(e) => {
                        if started_at.elapsed() >= HEALTHY_RUN {
                            attempts = 0;
                            backoff_ms = INITIAL_BACKOFF_MS;
                        }
                        attempts += 1;

                        if attempts > MAX_RESTART_ATTEMPTS {
                            tracing::error!(
                                "[{}] Detector failed after {} attempts, giving up: {}",
                                name,
                                MAX_RESTART_ATTEMPTS,
                                e
                            );
                            break;
                        }

                        if *shutdown.borrow() {
                            tracing::info!("[{}] Shutdown in progress, not restarting: {}", name, e);
                            break;
                        }

                        tracing::warn!(
                            "[{}] Detector failed (attempt {}/{}): {}. Retrying in {}ms",
                            name,
                            attempts,
                            MAX_RESTART_ATTEMPTS,
                            e,
                            backoff_ms
                        );

                        tokio::select! {
                            _ = sleep(Duration::from_millis(backoff_ms)) => {}
                            _ = shutdown.wait_for(|stop| *stop) => {
                                tracing::info!("[{}] Shutdown during restart backoff, not restarting", name);
                                break;
                            }
                        }
                        backoff_ms = (backoff_ms * 2).min(MAX_BACKOFF_MS);
                    }
                }
            }
        });

        self.handles.lock().await.push((detector_name, handle));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static STARTS: AtomicUsize = AtomicUsize::new(0);

    /// Panics on its first run, then behaves until shutdown.
    #[derive(Clone)]
    struct FlakyDetector;

    impl Detector for FlakyDetector {
        fn name(&self) -> &'static str {
            "flaky"
        }

        fn new(_event_tx: mpsc::Sender<Event>) -> Self {
            Self
        }

        async fn start(self: Arc<Self>, _ctx: DetectorContext, mut shutdown: watch::Receiver<bool>) -> anyhow::Result<()> {
            if STARTS.fetch_add(1, Ordering::SeqCst) == 0 {
                panic!("boom");
            }
            let _ = shutdown.wait_for(|stop| *stop).await;
            Ok(())
        }

        async fn stop(&self) {}
    }

    #[tokio::test]
    async fn restarts_a_detector_that_panicked() {
        let (event_tx, _event_rx) = mpsc::channel(1);
        let registry = DetectorRegistry::new(event_tx.clone());
        let ctx = DetectorContext {
            event_tx,
            config: crate::config::SharedConfig::new(crate::config::Config::default()),
            session_bus: zbus::Connection::session().await.unwrap(),
            system_bus: zbus::Connection::session().await.unwrap(),
        };

        registry.spawn_with_supervision::<FlakyDetector>(ctx, registry.shutdown_tx.subscribe()).await;
        tokio::time::sleep(Duration::from_millis(INITIAL_BACKOFF_MS + 300)).await;
        assert_eq!(STARTS.load(Ordering::SeqCst), 2, "detector was not restarted after panicking");

        registry.shutdown().await;
    }
}
