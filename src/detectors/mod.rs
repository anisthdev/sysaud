use std::sync::Arc;
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

#[derive(Clone)]
pub struct DetectorRegistry {
    event_tx: mpsc::Sender<Event>,
    shutdown_tx: watch::Sender<bool>,
    handles: Arc<tokio::sync::Mutex<Vec<(&'static str, tokio::task::JoinHandle<()>)>>>,
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
                let result = Arc::new(detector).start(ctx.clone(), shutdown_rx).await;

                match result {
                    Ok(()) => {
                        tracing::info!("[{}] Detector stopped gracefully", name);
                        tracing::debug!("DEBUG: [{}] Loop exiting", name);
                        break;
                    }
                    Err(e) => {
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
