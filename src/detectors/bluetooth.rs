use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::watch;
use futures_util::StreamExt;
use crate::event::Event;
use crate::dbus;
use crate::detectors::context::DetectorContext;
use crate::detectors::detector::Detector;
use crate::detectors::map_state::{MapTransition, StateMap};

const BLUEZ_BUS: &str = "org.bluez";
const BLUEZ_DEVICE_IFACE: &str = "org.bluez.Device1";
const BLUEZ_ADAPTER_IFACE: &str = "org.bluez.Adapter1";
const DBUS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

#[derive(Clone)]
pub struct BluetoothDetector {
    event_tx: mpsc::Sender<Event>,
    device_states: StateMap<String, bool>,
    adapter_states: StateMap<String, bool>,
}

impl Detector for BluetoothDetector {
    fn name(&self) -> &'static str {
        "bluetooth"
    }

    fn new(event_tx: mpsc::Sender<Event>) -> Self {
        Self {
            event_tx,
            device_states: StateMap::new(),
            adapter_states: StateMap::new(),
        }
    }

    async fn start(self: Arc<Self>, ctx: DetectorContext, mut shutdown: watch::Receiver<bool>) -> anyhow::Result<()> {
        let rule = zbus::MatchRule::builder()
            .interface("org.freedesktop.DBus.Properties")?
            .member("PropertiesChanged")?
            .build();
        let mut stream = dbus::matched_stream(&ctx.system_bus, vec![rule.clone()]).await?;

        tracing::info!("Bluetooth detector: monitoring BlueZ signals...");

        loop {
            tokio::select! {
                msg = stream.next() => {
                    let msg = match msg {
                        Some(Ok(m)) => m,
                        _ => break,
                    };

                    if !rule.matches(&msg).unwrap_or(false) {
                        continue;
                    }

                    let path = msg.header().path().map(|p| p.to_string()).unwrap_or_default();

                    if let Ok((interface, changed, _)) = msg.body().deserialize::<(String, HashMap<String, zvariant::OwnedValue>, Vec<String>)>() {
                        match interface.as_str() {
                            BLUEZ_DEVICE_IFACE => self.handle_device(&ctx.system_bus, &path, changed).await,
                            BLUEZ_ADAPTER_IFACE => self.handle_adapter(&ctx.system_bus, &path, changed).await,
                            _ => {}
                        }
                    }
                }
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        tracing::info!("Bluetooth detector: received shutdown signal");
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

impl BluetoothDetector {
    async fn handle_device(&self, bus: &zbus::Connection, path: &str, changed: HashMap<String, zvariant::OwnedValue>) {
        if let Some(value) = changed.get("Connected") {
            if let Ok(is_connected) = value.downcast_ref::<bool>() {
                if matches!(self.device_states.update(path.to_string(), is_connected), MapTransition::Unchanged) {
                    return;
                }

                let mut details = HashMap::new();
                details.insert("path".to_string(), path.to_string());

                if let Ok(alias) = self.get_property::<String>(bus, BLUEZ_BUS, path, BLUEZ_DEVICE_IFACE, "Alias").await {
                    details.insert("device".to_string(), alias);
                } else {
                    tracing::debug!("Bluetooth detector: failed to get Alias for {}", path);
                    details.insert("device".to_string(), "Unknown".to_string());
                }

                if let Ok(icon) = self.get_property::<String>(bus, BLUEZ_BUS, path, BLUEZ_DEVICE_IFACE, "Icon").await {
                    details.insert("icon".to_string(), icon);
                } else {
                    tracing::debug!("Bluetooth detector: failed to get Icon for {}", path);
                    details.insert("icon".to_string(), "unknown".to_string());
                }

                let name = if is_connected { "device-connected" } else { "device-disconnected" };
                tracing::info!("Bluetooth detector: emitting event bluetooth.{}", name);
                let event = Event::new("bluetooth", name, details);
                let _ = self.event_tx.send(event).await;
            }
        }
    }

    async fn handle_adapter(&self, bus: &zbus::Connection, path: &str, changed: HashMap<String, zvariant::OwnedValue>) {
        if let Some(value) = changed.get("Powered") {
            if let Ok(powered) = value.downcast_ref::<bool>() {
                if matches!(self.adapter_states.update(path.to_string(), powered), MapTransition::Unchanged) {
                    return;
                }

                let mut details = HashMap::new();
                details.insert("path".to_string(), path.to_string());

                if let Ok(alias) = self.get_property::<String>(bus, BLUEZ_BUS, path, BLUEZ_ADAPTER_IFACE, "Alias").await {
                    details.insert("adapter".to_string(), alias);
                } else {
                    details.insert("adapter".to_string(), "Adapter".to_string());
                }

                let name = if powered { "adapter-on" } else { "adapter-off" };
                tracing::info!("Bluetooth detector: emitting event bluetooth.{}", name);
                let event = Event::new("bluetooth", name, details);
                let _ = self.event_tx.send(event).await;
            }
        }
    }

    async fn get_property<T: serde::de::DeserializeOwned + zvariant::Type + TryFrom<zvariant::OwnedValue, Error = zvariant::Error>>(
        &self,
        bus: &zbus::Connection,
        dest: &str,
        path: &str,
        iface: &str,
        prop: &str,
    ) -> zbus::Result<T> {
        // bluetoothd may be stopping (e.g. during system shutdown), so never wait on it unbounded.
        let lookup = async {
            let proxy: zbus::Proxy = zbus::proxy::Builder::new(bus)
                .destination(dest)?
                .path(path)?
                .interface(iface)?
                .cache_properties(zbus::proxy::CacheProperties::No)
                .build()
                .await?;

            proxy.get_property(prop).await
        };

        tokio::time::timeout(DBUS_TIMEOUT, lookup)
            .await
            .map_err(|_| zbus::Error::Failure(format!("{iface}.{prop} on {path} timed out after {DBUS_TIMEOUT:?}")))?
    }
}
