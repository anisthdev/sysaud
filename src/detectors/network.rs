use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::mpsc;
use tokio::sync::watch;
use futures_util::StreamExt;
use crate::event::Event;
use crate::detectors::context::DetectorContext;
use crate::detectors::detector::Detector;
use crate::detectors::state::{StateCell, StateTransition};

const NM_BUS: &str = "org.freedesktop.NetworkManager";
const NM_PATH: &str = "/org/freedesktop/NetworkManager";
const NM_IFACE: &str = "org.freedesktop.NetworkManager";
const NM_ACTIVE_CONN_IFACE: &str = "org.freedesktop.NetworkManager.Connection.Active";
const NM_VPN_CONN_IFACE: &str = "org.freedesktop.NetworkManager.VPN.Connection";

const NM_STATE_DISCONNECTED: u32 = 20;
const NM_STATE_CONNECTED_GLOBAL: u32 = 70;

const VPN_STATE_ACTIVATED: u32 = 5;
const VPN_STATE_FAILED: u32 = 6;
const VPN_STATE_DISCONNECTED: u32 = 7;

#[derive(Clone)]
pub struct NetworkDetector {
    event_tx: mpsc::Sender<Event>,
    state: StateCell<String>,
}

impl Detector for NetworkDetector {
    fn name(&self) -> &'static str {
        "network"
    }

    fn new(event_tx: mpsc::Sender<Event>) -> Self {
        Self { event_tx, state: StateCell::new() }
    }

    async fn start(self: Arc<Self>, ctx: DetectorContext, mut shutdown: watch::Receiver<bool>) -> anyhow::Result<()> {
        let dbus_proxy = zbus::fdo::DBusProxy::new(&ctx.system_bus).await?;

        let rule = zbus::MatchRule::builder()
            .interface(NM_IFACE)?
            .member("StateChanged")?
            .path(NM_PATH)?
            .build();

        dbus_proxy.add_match_rule(rule).await?;

        let proxy: zbus::Proxy = zbus::Proxy::new(
            &ctx.system_bus,
            NM_BUS,
            NM_PATH,
            NM_IFACE,
        ).await?;

        let mut stream = proxy.receive_signal("StateChanged").await?;

        tracing::info!("Network detector: monitoring NetworkManager states...");

        loop {
            tokio::select! {
                biased;

                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        tracing::info!("Network detector: received shutdown signal");
                        break;
                    }
                }
                result = stream.next() => {
                    let signal = match result {
                        Some(s) => s,
                        None => break,
                    };
                    let nm_state: u32 = match signal.body().deserialize() {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::warn!("Failed to parse StateChanged: {}", e);
                            continue;
                        }
                    };

                    if nm_state == NM_STATE_CONNECTED_GLOBAL {
                        match self.state.update("connected".to_string()).await {
                            StateTransition::Unchanged => continue,
                            StateTransition::Initialized | StateTransition::Changed { .. } => {}
                        }

                        let mut details = HashMap::new();

                        match self.get_property::<zvariant::OwnedObjectPath>(&ctx.system_bus, NM_BUS, NM_PATH, NM_IFACE, "PrimaryConnection").await {
                            Ok(primary) => {
                                let primary_path = primary.as_str();
                                
                                if let Ok(conn_id) = self.get_property::<String>(&ctx.system_bus, NM_BUS, primary_path, NM_ACTIVE_CONN_IFACE, "Id").await {
                                    details.insert("name".to_string(), conn_id);
                                }

                                if let Ok(conn_type) = self.get_property::<String>(&ctx.system_bus, NM_BUS, primary_path, NM_ACTIVE_CONN_IFACE, "Type").await {
                                    details.insert("type".to_string(), conn_type);
                                }
                            },
                            Err(e) => {
                                tracing::warn!("Network detector: Failed to get connection details: {}", e);
                            }
                        }

                        let event_name = if details.get("type").map(|t| t.as_str()) == Some("802-11-wireless") {
                            "wifi-connected"
                        } else {
                            "network-connected"
                        };

                        tracing::info!("Network detector: Emitting event network.{}", event_name);
                        let event = Event::new("network", event_name, details);
                        let _ = self.event_tx.send(event).await;
                    } else if nm_state <= NM_STATE_DISCONNECTED {
                        match self.state.update("disconnected".to_string()).await {
                            StateTransition::Unchanged => continue,
                            StateTransition::Initialized | StateTransition::Changed { .. } => {}
                        }
                        tracing::info!("Network detector: Network disconnected");
                        let event = Event::new("network", "network-disconnected", HashMap::new());
                        let _ = self.event_tx.send(event).await;
                    }
                }
            }
        }

        self.stop().await;
        Ok(())
    }

    async fn stop(&self) {}
}

impl NetworkDetector {
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

#[derive(Clone)]
pub struct VpnDetector {
    event_tx: mpsc::Sender<Event>,
    vpn_states: Arc<Mutex<HashMap<String, u32>>>,
}

impl Detector for VpnDetector {
    fn name(&self) -> &'static str {
        "vpn"
    }

    fn new(event_tx: mpsc::Sender<Event>) -> Self {
        Self { event_tx, vpn_states: Arc::new(Mutex::new(HashMap::new())) }
    }

    async fn start(self: Arc<Self>, ctx: DetectorContext, mut shutdown: watch::Receiver<bool>) -> anyhow::Result<()> {
        let dbus_proxy = zbus::fdo::DBusProxy::new(&ctx.system_bus).await?;

        let rule = zbus::MatchRule::builder()
            .interface(NM_VPN_CONN_IFACE)?
            .member("VpnStateChanged")?
            .build();

        dbus_proxy.add_match_rule(rule).await?;

        let proxy: zbus::Proxy = zbus::Proxy::new(
            &ctx.system_bus,
            NM_BUS,
            "/org/freedesktop/NetworkManager/VPN",
            NM_VPN_CONN_IFACE,
        ).await?;

        let mut stream = proxy.receive_signal("VpnStateChanged").await?;

        tracing::info!("VPN detector: monitoring NetworkManager VPN states...");

        loop {
            tokio::select! {
                biased;

                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        tracing::info!("VPN detector: received shutdown signal");
                        break;
                    }
                }
                signal = stream.next() => {
                    let signal = match signal {
                        Some(s) => s,
                        None => break,
                    };
                    let (state, _reason): (u32, u32) = match signal.body().deserialize() {
                        Ok(body) => body,
                        Err(e) => {
                            tracing::warn!("Failed to parse VpnStateChanged body: {}", e);
                            continue;
                        }
                    };

                    let path = signal.header().path().map(|p| p.to_string()).unwrap_or_default();

                    let mut vpn_states = self.vpn_states.lock().await;
                    if vpn_states.get(&path) == Some(&state) {
                        continue;
                    }
                    vpn_states.insert(path.clone(), state);
                    drop(vpn_states);

                    let mut details = HashMap::new();
                    if let Ok(name) = self.get_property::<String>(&ctx.system_bus, NM_BUS, path.as_str(), NM_ACTIVE_CONN_IFACE, "Id").await {
                        details.insert("name".to_string(), name);
                    }

                    match state {
                        VPN_STATE_ACTIVATED => {
                            tracing::info!("VPN detector: VPN connected: {:?}", details.get("name"));
                            let event = Event::new("network", "vpn-connected", details);
                            let _ = self.event_tx.send(event).await;
                        }
                        VPN_STATE_DISCONNECTED | VPN_STATE_FAILED => {
                            tracing::info!("VPN detector: VPN disconnected: {:?}", details.get("name"));
                            if state == VPN_STATE_FAILED {
                                details.insert("failed".to_string(), "true".to_string());
                            }
                            let event = Event::new("network", "vpn-disconnected", details);
                            let _ = self.event_tx.send(event).await;
                        }
                        _ => {}
                    }
                }
            }
        }

        self.stop().await;
        Ok(())
    }

    async fn stop(&self) {}
}

impl VpnDetector {
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
