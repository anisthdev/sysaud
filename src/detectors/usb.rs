use std::collections::HashMap;
use std::os::unix::io::AsRawFd;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::sync::watch;
use tokio::task;
use tokio::time as tokio_time;
use mio::unix::SourceFd;
use mio::{Events, Interest, Poll, Token};
use crate::event::Event;
use crate::detectors::context::DetectorContext;
use crate::detectors::detector::Detector;

#[derive(Clone)]
pub struct UsbDetector {
    event_tx: mpsc::Sender<Event>,
}

impl Detector for UsbDetector {
    fn name(&self) -> &'static str {
        "usb"
    }

    fn new(event_tx: mpsc::Sender<Event>) -> Self {
        Self { event_tx }
    }

    async fn start(self: Arc<Self>, _ctx: DetectorContext, mut shutdown: watch::Receiver<bool>) -> anyhow::Result<()> {
        let (tx, mut rx) = mpsc::channel::<(String, String, String, std::path::PathBuf)>(100);
        let tx_clone = tx.clone();
        let mut tx = Some(tx);
        let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let running_thread = running.clone();

        std::thread::spawn(move || {
            let monitor = match udev::MonitorBuilder::new() {
                Ok(m) => m,
                Err(e) => {
                    tracing::error!("USB detector: failed to create monitor: {}", e);
                    return;
                }
            };

            let socket = match monitor.match_subsystem("usb") {
                Ok(m) => match m.listen() {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::error!("USB detector: failed to listen: {}", e);
                        return;
                    }
                },
                Err(e) => {
                    tracing::error!("USB detector: failed to match subsystem: {}", e);
                    return;
                }
            };

            let fd = socket.as_raw_fd();
            tracing::info!("USB detector: monitoring udev for USB devices (fd: {})", fd);

            let mut poll = match Poll::new() {
                Ok(p) => p,
                Err(e) => {
                    tracing::error!("USB detector: failed to create poll: {}", e);
                    return;
                }
            };

            let mut source = SourceFd(&fd);
            if let Err(e) = poll.registry().register(&mut source, Token(0), Interest::READABLE) {
                tracing::error!("USB detector: failed to register source: {}", e);
                return;
            }

            let mut events = Events::with_capacity(10);

            tracing::info!("USB detector: starting poll loop");

            loop {
                if !running_thread.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }
                match poll.poll(&mut events, Some(Duration::from_secs(1))) {
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!("USB detector: poll error: {}", e);
                        break;
                    }
                }

                for event in &events {
                    if event.is_readable() {
                        tracing::debug!("USB detector: socket is readable, reading event");

                        for _ in 0..10 {
                            let event = socket.iter().next();
                            match event {
                                Some(event) => {
                                    let event_type = event.event_type();
                                    tracing::debug!("USB detector: received event type={:?}", event_type);

                                    if event_type != udev::EventType::Add && event_type != udev::EventType::Remove {
                                        tracing::debug!("USB detector: ignoring non add/remove event");
                                        continue;
                                    }

                                    let device = event.device();
                                    tracing::debug!("USB detector: device={:?}", device.syspath());

                                    if device.property_value("DEVTYPE").map(|s| s != "usb_device").unwrap_or(true) {
                                        tracing::debug!("USB detector: skipping non-usb_device devtype");
                                        continue;
                                    }

                                    let vendor = device.property_value("ID_VENDOR_FROM_DATABASE")
                                        .or_else(|| device.property_value("ID_VENDOR"))
                                        .map(|s| s.to_string_lossy().into_owned())
                                        .unwrap_or_else(|| "unknown".to_string());

                                    let model = device.property_value("ID_MODEL_FROM_DATABASE")
                                        .or_else(|| device.property_value("ID_MODEL"))
                                        .map(|s| s.to_string_lossy().into_owned())
                                        .unwrap_or_else(|| "unknown".to_string());

                                    let sys_path = device.syspath().to_path_buf();
                                    let action = event_type;

                                    tracing::info!("USB detector: {} - {} {} ({})",
                                        if action == udev::EventType::Add { "plugged" } else { "unplugged" },
                                        vendor, model, sys_path.display());

                                    if tx_clone.try_send((vendor, model, action.to_string(), sys_path)).is_err() {
                                        tracing::warn!("USB detector: channel send failed");
                                    }
                                }
                                None => {
                                    break;
                                }
                            }
                        }
                    }
                }
            }
            tracing::info!("USB detector: poll loop ended");
        });

        loop {
            tokio::select! {
                msg = rx.recv() => {
                    let (vendor, model, action, sys_path) = match msg {
                        Some(m) => m,
                        None => break,
                    };
                    tracing::debug!("USB detector: received from channel: {} {} {}", vendor, model, action);
                    let tx = self.event_tx.clone();

                    if action == "add" {
                        tracing::debug!("USB detector: scheduling plug event for {} {}", vendor, model);
                        let sys_path_clone = sys_path.clone();
                        let tx_clone = tx.clone();
                        let vendor_clone = vendor.clone();
                        let model_clone = model.clone();
                        let mut shutdown_plug = shutdown.clone();
                        tokio::spawn(async move {
                            tokio::select! {
                                _ = tokio_time::sleep(Duration::from_millis(500)) => {
                                    let dev_type = task::spawn_blocking(move || {
                                        classify_usb_device_from_syspath(&sys_path_clone)
                                    }).await.unwrap_or_else(|_| "device".to_string());
                                    tracing::info!("USB detector: emitting device-plugged {} {} type={}", vendor_clone, model_clone, dev_type);
                                    let event = Event::new("usb", "device-plugged", {
                                        let mut m = HashMap::new();
                                        m.insert("vendor".to_string(), vendor_clone);
                                        m.insert("model".to_string(), model_clone);
                                        m.insert("type".to_string(), dev_type);
                                        m
                                    });
                                    let _ = tx_clone.send(event).await;
                                }
                                _ = shutdown_plug.changed() => {
                                    if *shutdown_plug.borrow() {
                                        tracing::debug!("DEBUG: USB delayed plug task exiting");
                                    }
                                }
                            }
                        });
                    } else {
                        tracing::debug!("USB detector: emitting device-unplugged for {} {}", vendor, model);
                        let dev_type = task::spawn_blocking({
                            let sys_path = sys_path.clone();
                            move || classify_usb_device_from_syspath(&sys_path)
                        }).await?;
                        tracing::info!("USB detector: emitting device-unplugged {} {} type={}", vendor, model, dev_type);
                        let event = Event::new("usb", "device-unplugged", {
                            let mut m = HashMap::new();
                            m.insert("vendor".to_string(), vendor);
                            m.insert("model".to_string(), model);
                            m.insert("type".to_string(), dev_type);
                            m
                        });
                        let _ = self.event_tx.send(event).await;
                    }
                }
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        tracing::info!("USB detector: received shutdown signal");
                        running.store(false, std::sync::atomic::Ordering::SeqCst);
                        drop(tx.take());
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

const DEVICE_TYPE_CHECKS: &[(&str, &str)] = &[
    ("ID_INPUT_KEYBOARD", "keyboard"),
    ("ID_INPUT_MOUSE", "mouse"),
    ("ID_INPUT_JOYSTICK", "controller"),
    ("ID_MTP_DEVICE", "phone"),
    ("ID_PDA", "phone"),
];

fn classify_usb_device_from_syspath(sys_path: &std::path::Path) -> String {
    if let Some(device_type) = check_device_and_children(sys_path) {
        return device_type;
    }

    "device".to_string()
}

fn check_device_and_children(sys_path: &std::path::Path) -> Option<String> {
    if let Ok(device) = udev::Device::from_syspath(sys_path) {
        for (prop, label) in DEVICE_TYPE_CHECKS {
            if device.property_value(prop).is_some() {
                return Some(label.to_string());
            }
        }
    }

    if is_usb_storage_device(sys_path) {
        return Some("storage".to_string());
    }

    if let Ok(dir) = std::fs::read_dir(sys_path) {
        for entry in dir.filter_map(|e| e.ok()) {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = path.file_name()?.to_str()?;
            if name.starts_with('.') {
                continue;
            }

            if entry.path().symlink_metadata().map(|m| m.file_type().is_symlink()).unwrap_or(false) {
                continue;
            }

            if let Some(device_type) = check_device_and_children(&path) {
                return Some(device_type);
            }
        }
    }

    None
}

fn is_usb_storage_device(sys_path: &std::path::Path) -> bool {
    let driver_path = sys_path.join("driver");
    if driver_path.exists() {
        if let Ok(target) = std::fs::read_link(&driver_path) {
            if let Some(name) = target.file_name() {
                return name == "usb-storage";
            }
        }
    }
    false
}
