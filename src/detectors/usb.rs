use std::collections::HashMap;
use std::os::unix::io::AsRawFd;
use std::sync::Arc;
use std::time::{Duration, Instant};
use parking_lot::Mutex;
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
    devices: Arc<Mutex<HashMap<String, UsbDeviceInfo>>>,
}

#[derive(Debug, Clone)]
struct UsbDeviceInfo {
    vendor: String,
    model: String,
    device_type: String,
    cached_at: Instant,
}

#[derive(Debug, Clone)]
struct UsbMonitorEvent {
    key: String,
    vendor: String,
    model: String,
    action: String,
    sys_path: std::path::PathBuf,
}

const USB_CACHE_TTL: Duration = Duration::from_secs(12 * 60 * 60);
const USB_CACHE_MAX_ENTRIES: usize = 256;

impl Detector for UsbDetector {
    fn name(&self) -> &'static str {
        "usb"
    }

    fn new(event_tx: mpsc::Sender<Event>) -> Self {
        Self {
            event_tx,
            devices: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    async fn start(self: Arc<Self>, _ctx: DetectorContext, mut shutdown: watch::Receiver<bool>) -> anyhow::Result<()> {
        let (tx, mut rx) = mpsc::channel::<UsbMonitorEvent>(100);
        let tx_clone = tx.clone();
        let mut tx = Some(tx);
        let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let running_thread = running.clone();

        std::thread::spawn(move || run_usb_monitor(tx_clone, running_thread));

        loop {
            tokio::select! {
                msg = rx.recv() => {
                    let UsbMonitorEvent { key, vendor, model, action, sys_path } = match msg {
                        Some(m) => m,
                        None => break,
                    };
                    tracing::debug!("USB detector: received from channel: {} {} {}", vendor, model, action);
                    let tx = self.event_tx.clone();

                    if action == "add" {
                        tracing::debug!("USB detector: scheduling plug event for {} {}", vendor, model);
                        let devices = self.devices.clone();
                        let key_clone = key.clone();
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
                                    let info = UsbDeviceInfo {
                                        vendor: vendor_clone.clone(),
                                        model: model_clone.clone(),
                                        device_type: dev_type.clone(),
                                        cached_at: Instant::now(),
                                    };
                                    {
                                        let mut guard = devices.lock();
                                        prune_usb_cache(&mut guard);
                                        guard.insert(key_clone.clone(), info);
                                    }
                                    tracing::debug!("USB detector: cached metadata for {}", key_clone);
                                    tracing::info!("USB detector: emitting device-plugged {} {} type={}", vendor_clone, model_clone, dev_type);
                                    let event = Event::new("usb", "device-plugged", make_usb_detail(vendor_clone, model_clone, dev_type));
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
                        let fallback = UsbDeviceInfo {
                            vendor,
                            model,
                            device_type: "device".to_string(),
                            cached_at: Instant::now(),
                        };
                        let info = {
                            let mut guard = self.devices.lock();
                            prune_usb_cache(&mut guard);
                            guard.remove(&key)
                        };
                        let info = match info {
                            Some(info) => {
                                tracing::debug!("USB detector: using cached metadata for {}", key);
                                info
                            }
                            None => {
                                tracing::debug!("USB detector: cache miss for {}, using fallback metadata", key);
                                fallback
                            }
                        };
                        tracing::info!(
                            "USB detector: emitting device-unplugged {} {} type={}",
                            info.vendor,
                            info.model,
                            info.device_type
                        );
                        let event = Event::new(
                            "usb",
                            "device-unplugged",
                            make_usb_detail(info.vendor, info.model, info.device_type),
                        );
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

fn run_usb_monitor(
    tx_clone: mpsc::Sender<UsbMonitorEvent>,
    running_thread: Arc<std::sync::atomic::AtomicBool>,
) {
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
                            let key = sys_path.to_string_lossy().into_owned();
                            let action = event_type;

                            tracing::info!("USB detector: {} - {} {} ({})",
                                if action == udev::EventType::Add { "plugged" } else { "unplugged" },
                                vendor, model, sys_path.display());

                            if tx_clone.try_send(UsbMonitorEvent {
                                key,
                                vendor,
                                model,
                                action: action.to_string(),
                                sys_path,
                            }).is_err() {
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
}

const DEVICE_TYPE_CHECKS: &[(&str, &str)] = &[
    ("ID_INPUT_KEYBOARD", "keyboard"),
    ("ID_INPUT_MOUSE", "mouse"),
    ("ID_INPUT_JOYSTICK", "controller"),
    ("ID_MTP_DEVICE", "phone"),
    ("ID_PDA", "phone"),
];

fn make_usb_detail(vendor: String, model: String, device_type: String) -> HashMap<String, String> {
    let mut detail = HashMap::new();
    detail.insert("vendor".to_string(), vendor);
    detail.insert("model".to_string(), model);
    detail.insert("type".to_string(), device_type);
    detail
}

fn prune_usb_cache(cache: &mut HashMap<String, UsbDeviceInfo>) {
    let now = Instant::now();
    cache.retain(|_, info| now.duration_since(info.cached_at) < USB_CACHE_TTL);

    if cache.len() <= USB_CACHE_MAX_ENTRIES {
        return;
    }

    let mut oldest_key = None;
    let mut oldest_time = now;

    for (key, info) in cache.iter() {
        if info.cached_at <= oldest_time {
            oldest_time = info.cached_at;
            oldest_key = Some(key.clone());
        }
    }

    if let Some(key) = oldest_key {
        tracing::debug!("USB detector: evicting stale cache entry {}", key);
        cache.remove(&key);
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unplug_uses_cached_metadata_when_available() {
        let mut cache = HashMap::new();
        cache.insert(
            "/devices/mock/usb1".to_string(),
            UsbDeviceInfo {
                vendor: "Logitech".to_string(),
                model: "MX Master".to_string(),
                device_type: "mouse".to_string(),
                cached_at: Instant::now(),
            },
        );

        let fallback = UsbDeviceInfo {
            vendor: "unknown".to_string(),
            model: "unknown".to_string(),
            device_type: "device".to_string(),
            cached_at: Instant::now(),
        };

        let info = cache.remove("/devices/mock/usb1").unwrap_or(fallback);
        let detail = make_usb_detail(info.vendor, info.model, info.device_type);

        assert_eq!(detail.get("vendor").map(String::as_str), Some("Logitech"));
        assert_eq!(detail.get("model").map(String::as_str), Some("MX Master"));
        assert_eq!(detail.get("type").map(String::as_str), Some("mouse"));
        assert!(cache.is_empty());
    }

    #[test]
    fn unplug_falls_back_when_cache_misses() {
        let mut cache: HashMap<String, UsbDeviceInfo> = HashMap::new();
        let fallback = UsbDeviceInfo {
            vendor: "unknown".to_string(),
            model: "unknown".to_string(),
            device_type: "device".to_string(),
            cached_at: Instant::now(),
        };

        let info = cache.remove("/devices/mock/usb1").unwrap_or(fallback);
        let detail = make_usb_detail(info.vendor, info.model, info.device_type);

        assert_eq!(detail.get("vendor").map(String::as_str), Some("unknown"));
        assert_eq!(detail.get("model").map(String::as_str), Some("unknown"));
        assert_eq!(detail.get("type").map(String::as_str), Some("device"));
    }

    #[test]
    fn prune_removes_expired_entries() {
        let mut cache = HashMap::new();
        cache.insert(
            "/devices/mock/usb1".to_string(),
            UsbDeviceInfo {
                vendor: "Old".to_string(),
                model: "Entry".to_string(),
                device_type: "device".to_string(),
                cached_at: Instant::now() - USB_CACHE_TTL - Duration::from_secs(1),
            },
        );

        prune_usb_cache(&mut cache);

        assert!(cache.is_empty());
    }
}
