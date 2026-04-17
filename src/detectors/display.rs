use std::fs;
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::sync::watch;
use tokio::task;
use parking_lot::Mutex;
use mio::unix::SourceFd;
use mio::{Events, Interest, Poll, Token};
use crate::event::Event;
use crate::detectors::context::DetectorContext;
use crate::detectors::detector::Detector;

#[derive(Clone)]
pub struct DisplayDetector {
    event_tx: mpsc::Sender<Event>,
    backlight_device: Arc<Mutex<Option<String>>>,
}

impl Detector for DisplayDetector {
    fn name(&self) -> &'static str {
        "display"
    }

    fn new(event_tx: mpsc::Sender<Event>) -> Self {
        Self {
            event_tx,
            backlight_device: Arc::new(Mutex::new(None)),
        }
    }

    async fn start(self: Arc<Self>, _ctx: DetectorContext, mut shutdown: watch::Receiver<bool>) -> anyhow::Result<()> {
        let backlight_path = task::spawn_blocking(Self::find_backlight_device_static).await?;
        {
            let mut device = self.backlight_device.lock();
            *device = backlight_path;
        }

        let (backlight_tx, mut backlight_rx) = mpsc::channel::<()>(10);
        let backlight_tx_clone = backlight_tx.clone();
        let mut backlight_tx = Some(backlight_tx);

        {
            let has_backlight = self.backlight_device.lock().is_some();
            if has_backlight {
                let _backlight_device = self.backlight_device.clone();
                
                std::thread::spawn(move || {
                    let monitor = match udev::MonitorBuilder::new() {
                        Ok(m) => m,
                        Err(e) => {
                            tracing::error!("Display detector: backlight failed to create monitor: {}", e);
                            return;
                        }
                    };

                    let socket = match monitor.match_subsystem("backlight") {
                        Ok(m) => match m.listen() {
                            Ok(s) => s,
                            Err(e) => {
                                tracing::error!("Display detector: backlight failed to listen: {}", e);
                                return;
                            }
                        },
                        Err(e) => {
                            tracing::error!("Display detector: backlight failed to match: {}", e);
                            return;
                        }
                    };

                    tracing::info!("Display detector: monitoring backlight");

                    let mut poll = match Poll::new() {
                        Ok(p) => p,
                        Err(e) => {
                            tracing::error!("Display detector: backlight poll create failed: {}", e);
                            return;
                        }
                    };

                    let fd = socket.as_raw_fd();
                    let mut source = SourceFd(&fd);
                    if let Err(e) = poll.registry().register(&mut source, Token(0), Interest::READABLE) {
                        tracing::error!("Display detector: backlight register failed: {}", e);
                        return;
                    }

                    let mut events = Events::with_capacity(10);

                    loop {
                        match poll.poll(&mut events, Some(Duration::from_secs(1))) {
                            Ok(_) => {}
                            Err(e) => {
                                tracing::warn!("Display detector: backlight poll error: {}", e);
                                break;
                            }
                        }

                        for _ in &events {
                            for _ in 0..10 {
                                let next = socket.iter().next();
                                match next {
                                    Some(event) => {
                                        if event.event_type() != udev::EventType::Change {
                                            continue;
                                        }
                                        let _ = backlight_tx_clone.try_send(());
                                    }
                                    None => break,
                                }
                            }
                        }
                    }
                });
            }
        }

        loop {
            tokio::select! {
                _ = backlight_rx.recv() => {
                    let device = self.backlight_device.lock().clone();
                    if let Some(ref path) = device {
                        let path_clone = path.clone();
                        if let Some(level) = task::spawn_blocking(move || read_backlight_level(&path_clone)).await? {
                            tracing::info!("Display detector: brightness changed to {}%", level);
                            let event = Event::new(
                                "display",
                                "brightness-changed",
                                {
                                    let mut m = std::collections::HashMap::new();
                                    m.insert("level".to_string(), level.to_string());
                                    m.insert("device".to_string(), path.clone());
                                    m
                                },
                            );
                            let _ = self.event_tx.send(event).await;
                        }
                    }
                }
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        tracing::info!("Display detector: received shutdown signal");
                        drop(backlight_tx.take());
                        break;
                    }
                }
            }
        }

        self.stop().await;
        Ok(())
    }

    async fn stop(&self) {}
}

impl DisplayDetector {
    fn find_backlight_device_static() -> Option<String> {
        let _context = match udev::Udev::new() {
            Ok(c) => c,
            Err(_) => return None,
        };

        let mut enumerator = match udev::Enumerator::new() {
            Ok(e) => e,
            Err(_) => return None,
        };

        if enumerator.match_subsystem("backlight").is_err() {
            return None;
        }

        for device in enumerator.scan_devices().ok()? {
            let sysname = device.sysname().to_string_lossy();
            let path = format!("/sys/class/backlight/{sysname}");
            if Path::new(&path).exists() {
                tracing::info!("Display detector: found backlight at {}", path);
                return Some(path);
            }
        }
        None
    }
}

fn read_backlight_level(device: &str) -> Option<i32> {
    let brightness_path = Path::new(device).join("brightness");
    let max_brightness_path = Path::new(device).join("max_brightness");

    let brightness: i32 = fs::read_to_string(&brightness_path)
        .ok()?
        .trim().parse().ok()?;
    let max_brightness: i32 = fs::read_to_string(&max_brightness_path)
        .ok()?
        .trim().parse().ok()?;

    if max_brightness == 0 {
        return None;
    }

    Some((brightness as f64 / max_brightness as f64 * 100.0).round() as i32)
}
