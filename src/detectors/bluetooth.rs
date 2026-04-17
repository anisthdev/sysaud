use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::mpsc;
use tokio::sync::watch;
use futures_util::StreamExt;
use zbus::MessageStream;
use crate::event::Event;
use crate::detectors::context::DetectorContext;
use crate::detectors::detector::Detector;

const BLUEZ_BUS: &str = "org.bluez";
const BLUEZ_DEVICE_IFACE: &str = "org.bluez.Device1";
const BLUEZ_ADAPTER_IFACE: &str = "org.bluez.Adapter1";

#[derive(Clone)]
pub struct BluetoothDetector {
    event_tx: mpsc::Sender<Event>,
    device_states: Arc<Mutex<HashMap<String, bool>>>,
    adapter_states: Arc<Mutex<HashMap<String, bool>>>,
}

impl Detector for BluetoothDetector {
    fn name(&self) -> &'static str {
        "bluetooth"
    }

    fn new(event_tx: mpsc::Sender<Event>) -> Self {
        Self {
            event_tx,
            device_states: Arc::new(Mutex::new(HashMap::new())),
            adapter_states: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    async fn start(self: Arc<Self>, ctx: DetectorContext, mut shutdown: watch::Receiver<bool>) -> anyhow::Result<()> {
        let dbus_proxy = zbus::fdo::DBusProxy::new(&ctx.system_bus).await?;

        let rule = zbus::MatchRule::builder()
            .interface("org.freedesktop.DBus.Properties")?
            .member("PropertiesChanged")?
            .build();

        dbus_proxy.add_match_rule(rule.clone()).await?;

        let mut stream = MessageStream::from(&ctx.system_bus);

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
                let mut states = self.device_states.lock().await;
                if states.get(path) == Some(&is_connected) {
                    return;
                }
                states.insert(path.to_string(), is_connected);
                drop(states);

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
                let mut states = self.adapter_states.lock().await;
                if states.get(path) == Some(&powered) {
                    return;
                }
                states.insert(path.to_string(), powered);
                drop(states);

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
        let proxy: zbus::Proxy = zbus::Proxy::new(
            bus,
            dest,
            path,
            iface,
        ).await?;

        proxy.get_property(prop).await
    }
}
