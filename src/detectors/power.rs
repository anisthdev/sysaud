use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::mpsc;
use tokio::sync::watch;
use futures_util::StreamExt;
use crate::event::Event;
use crate::detectors::context::DetectorContext;
use crate::detectors::detector::Detector;

const UPOWER_BUS: &str = "org.freedesktop.UPower";
const UPOWER_PATH: &str = "/org/freedesktop/UPower";
const UPOWER_IFACE: &str = "org.freedesktop.UPower";
const UPOWER_DEVICE_IFACE: &str = "org.freedesktop.UPower.Device";
const DISPLAY_DEVICE_PATH: &str = "/org/freedesktop/UPower/devices/DisplayDevice";

const WARNING_LOW: u32 = 3;
const WARNING_CRITICAL: u32 = 4;
const WARNING_ACTION: u32 = 5;

#[derive(Clone)]
pub struct PowerDetector {
    event_tx: mpsc::Sender<Event>,
    on_battery: Arc<Mutex<Option<bool>>>,
}

impl Detector for PowerDetector {
    fn name(&self) -> &'static str {
        "power"
    }

    fn new(event_tx: mpsc::Sender<Event>) -> Self {
        Self { event_tx, on_battery: Arc::new(Mutex::new(None)) }
    }

    async fn start(self: Arc<Self>, ctx: DetectorContext, mut shutdown: watch::Receiver<bool>) -> anyhow::Result<()> {
        let proxy: zbus::Proxy = zbus::Proxy::new(
            &ctx.system_bus,
            UPOWER_BUS,
            UPOWER_PATH,
            "org.freedesktop.DBus.Properties",
        ).await?;

        let mut stream = proxy.receive_signal("PropertiesChanged").await?;

        tracing::info!("Power detector: subscribed to UPower signals");

        loop {
            tokio::select! {
                biased;

                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        tracing::info!("Power detector: received shutdown signal");
                        break;
                    }
                }
                signal = stream.next() => {
                    let signal = match signal {
                        Some(s) => s,
                        None => break,
                    };
                    let (interface, changed, _invalidated): (
                        String,
                        HashMap<String, zvariant::OwnedValue>,
                        Vec<String>,
                    ) = match signal.body().deserialize() {
                        Ok(body) => body,
                        Err(e) => {
                            tracing::warn!("Failed to parse PropertiesChanged body: {}", e);
                            continue;
                        }
                    };

                    if interface != UPOWER_IFACE {
                        continue;
                    }

                    if let Some(value) = changed.get("OnBattery") {
                        if let Ok(is_on_battery) = value.downcast_ref::<bool>() {
                            let mut on_battery = self.on_battery.lock().await;
                            if *on_battery != Some(is_on_battery) {
                                *on_battery = Some(is_on_battery);
                                let name = if is_on_battery {
                                    "ac-unplugged"
                                } else {
                                    "ac-plugged"
                                };
                                let event = Event::new("power", name, HashMap::new());
                                let _ = self.event_tx.send(event).await;
                            }
                        }
                    }
                }
            }
        }

        self.stop().await;
        Ok(())
    }

    async fn stop(&self) {}
}

#[derive(Clone)]
pub struct BatteryDetector {
    event_tx: mpsc::Sender<Event>,
    last_warning: Arc<Mutex<Option<u32>>>,
    last_pct_bucket: Arc<Mutex<Option<i32>>>,
}

impl Detector for BatteryDetector {
    fn name(&self) -> &'static str {
        "battery"
    }

    fn new(event_tx: mpsc::Sender<Event>) -> Self {
        Self {
            event_tx,
            last_warning: Arc::new(Mutex::new(None)),
            last_pct_bucket: Arc::new(Mutex::new(None)),
        }
    }

    async fn start(self: Arc<Self>, ctx: DetectorContext, mut shutdown: watch::Receiver<bool>) -> anyhow::Result<()> {
        let proxy: zbus::Proxy = zbus::Proxy::new(
            &ctx.system_bus,
            UPOWER_BUS,
            DISPLAY_DEVICE_PATH,
            "org.freedesktop.DBus.Properties",
        ).await?;

        let mut stream = proxy.receive_signal("PropertiesChanged").await?;

        tracing::info!("Battery detector: subscribed to UPower battery signals");

        loop {
            tokio::select! {
                biased;

                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        tracing::info!("Battery detector: received shutdown signal");
                        break;
                    }
                }
                signal = stream.next() => {
                    let signal = match signal {
                        Some(s) => s,
                        None => break,
                    };
                    let (interface, changed, _invalidated): (
                        String,
                        HashMap<String, zvariant::OwnedValue>,
                        Vec<String>,
                    ) = match signal.body().deserialize() {
                        Ok(body) => body,
                        Err(e) => {
                            tracing::warn!("Failed to parse PropertiesChanged body: {}", e);
                            continue;
                        }
                    };

                    if interface != UPOWER_DEVICE_IFACE {
                        continue;
                    }

                    if let Some(value) = changed.get("WarningLevel") {
                        if let Ok(level) = value.downcast_ref::<u32>() {
                            let mut last_warning = self.last_warning.lock().await;
                            if *last_warning != Some(level) && level >= WARNING_LOW {
                                *last_warning = Some(level);
                                drop(last_warning);

                                let name = match level {
                                    WARNING_LOW => "battery-low",
                                    WARNING_CRITICAL => "battery-critical",
                                    WARNING_ACTION => "battery-action",
                                    _ => continue,
                                };

                                let mut details = HashMap::new();
                                if let Ok(pct) = self.get_property::<f64>(&ctx.system_bus, UPOWER_BUS, DISPLAY_DEVICE_PATH, UPOWER_DEVICE_IFACE, "Percentage").await {
                                    details.insert("percentage".to_string(), pct.to_string());
                                }

                                let event = Event::new("power", name, details);
                                let _ = self.event_tx.send(event).await;
                            }
                        }
                    }

                    if let Some(value) = changed.get("Percentage") {
                        if let Ok(pct) = value.downcast_ref::<f64>() {
                            let bucket = (pct as i32) / 10;
                            let mut last_bucket = self.last_pct_bucket.lock().await;
                            if *last_bucket != Some(bucket) {
                                let old_bucket = *last_bucket;
                                *last_bucket = Some(bucket);

                                if old_bucket.is_some() {
                                    drop(last_bucket);

                                    let direction = if bucket > old_bucket.unwrap() {
                                        "charging"
                                    } else {
                                        "discharging"
                                    };

                                    let mut details = HashMap::new();
                                    details.insert("percentage".to_string(), pct.to_string());
                                    details.insert("direction".to_string(), direction.to_string());

                                    let event = Event::new("power", "battery-level", details);
                                    let _ = self.event_tx.send(event).await;
                                }
                            }
                        }
                    }
                }
            }
        }

        self.stop().await;
        Ok(())
    }

    async fn stop(&self) {}
}

impl BatteryDetector {
    async fn get_property<T: serde::de::DeserializeOwned + zvariant::Type>(
        &self,
        bus: &zbus::Connection,
        dest: &str,
        path: &str,
        iface: &str,
        prop: &str,
    ) -> zbus::Result<T> {
        let proxy: zbus::Proxy = zbus::Proxy::new(
            bus,
            dest,
            path,
            "org.freedesktop.DBus.Properties",
        ).await?;

        let reply = proxy.call_method("Get", &(iface, prop)).await?;
        let value: T = reply.body().deserialize()?;
        Ok(value)
    }
}
