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
        let rule = zbus::MatchRule::builder()
            .interface(NM_IFACE)?
            .member("StateChanged")?
            .path(NM_PATH)?
            .build();
        dbus::register_match_rules(&ctx.system_bus, vec![rule]).await?;

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
    vpn_states: StateMap<String, u32>,
}

impl Detector for VpnDetector {
    fn name(&self) -> &'static str {
        "vpn"
    }

    fn new(event_tx: mpsc::Sender<Event>) -> Self {
        Self { event_tx, vpn_states: StateMap::new() }
    }

    async fn start(self: Arc<Self>, ctx: DetectorContext, mut shutdown: watch::Receiver<bool>) -> anyhow::Result<()> {
        let rule = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .interface(NM_VPN_CONN_IFACE)?
            .member("VpnStateChanged")?
            .build();
        let mut stream = zbus::MessageStream::for_match_rule(rule, &ctx.system_bus, None).await?;

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
                    let signal = match signal {
                        Ok(msg) => msg,
                        Err(e) => {
                            tracing::warn!("VPN detector: failed to read signal: {}", e);
                            continue;
                        }
                    };
                    let (state, _reason): (u32, u32) = match signal.body().deserialize() {
                        Ok(body) => body,
                        Err(e) => {
                            tracing::warn!("Failed to parse VpnStateChanged body: {}", e);
                            continue;
                        }
                    };

                    let path = signal.header().path().map(|p| p.to_string()).unwrap_or_default();

                    if matches!(self.vpn_states.update(path.clone(), state), MapTransition::Unchanged) {
                        continue;
                    }

                    let mut details = HashMap::new();
                    if let Some(name) = self.resolve_vpn_name(&ctx.system_bus, path.as_str()).await {
                        details.insert("name".to_string(), name);
                    }
                    if let Some(name) = details.get("name").cloned() {
                        details.insert("ifname".to_string(), name);
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
                            details.entry("ifname".to_string()).or_insert_with(|| "VPN".to_string());
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
    async fn resolve_vpn_name(&self, bus: &zbus::Connection, path: &str) -> Option<String> {
        if let Ok(name) = self
            .get_property::<String>(bus, NM_BUS, path, NM_ACTIVE_CONN_IFACE, "Id")
            .await
        {
            return Some(name);
        }

        let connection_path = self
            .get_property::<zvariant::OwnedObjectPath>(bus, NM_BUS, path, NM_ACTIVE_CONN_IFACE, "Connection")
            .await
            .ok()?;

        let proxy: zbus::Proxy = zbus::Proxy::new(
            bus,
            NM_BUS,
            connection_path.as_str(),
            "org.freedesktop.NetworkManager.Settings.Connection",
        )
        .await
        .ok()?;

        let reply = proxy.call_method("GetSettings", &()).await.ok()?;
        let settings: HashMap<String, HashMap<String, zvariant::OwnedValue>> = reply.body().deserialize().ok()?;
        let connection = settings.get("connection")?;
        connection.get("id")?.downcast_ref::<String>().ok()
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
