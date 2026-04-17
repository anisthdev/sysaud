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

const UDISKS_BUS: &str = "org.freedesktop.UDisks2";
const UDISKS_FS_IFACE: &str = "org.freedesktop.UDisks2.Filesystem";
const UDISKS_BLOCK_IFACE: &str = "org.freedesktop.UDisks2.Block";

#[derive(Clone)]
pub struct DiskDetector {
    event_tx: mpsc::Sender<Event>,
    mount_points: StateMap<String, Vec<String>>,
}

impl Detector for DiskDetector {
    fn name(&self) -> &'static str {
        "disk"
    }

    fn new(event_tx: mpsc::Sender<Event>) -> Self {
        Self {
            event_tx,
            mount_points: StateMap::new(),
        }
    }

    async fn start(self: Arc<Self>, ctx: DetectorContext, mut shutdown: watch::Receiver<bool>) -> anyhow::Result<()> {
        let rule = zbus::MatchRule::builder()
            .interface("org.freedesktop.DBus.Properties")?
            .member("PropertiesChanged")?
            .build();
        let mut stream = dbus::matched_stream(&ctx.system_bus, vec![rule.clone()]).await?;

        tracing::info!("Disk detector: subscribed to UDisks2 signals");

        loop {
            tokio::select! {
                biased;

                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        tracing::info!("Disk detector: received shutdown signal");
                        break;
                    }
                }
                msg = stream.next() => {
                    let msg = match msg {
                        Some(Ok(m)) => m,
                        _ => break,
                    };

                    if !rule.matches(&msg).unwrap_or(false) {
                        continue;
                    }

                    let (interface, changed, _): (String, HashMap<String, zvariant::OwnedValue>, Vec<String>) =
                        match msg.body().deserialize() {
                            Ok(b) => b,
                            Err(_) => continue,
                        };

                    if interface != UDISKS_FS_IFACE {
                        continue;
                    }

                    if !changed.contains_key("MountPoints") {
                        continue;
                    }

                    let path = msg.header().path().map(|p| p.to_string()).unwrap_or_default();
                    
                    let mount_points = decode_mount_points(&changed);
                    match self.mount_points.update(path.clone(), mount_points.clone()) {
                        MapTransition::Inserted => {
                            if !mount_points.is_empty() {
                                let label = get_block_property(&ctx.system_bus, &path, "IdLabel").await;
                                let device = get_block_property(&ctx.system_bus, &path, "Device").await;
                                tracing::info!("Disk detector: mounted {} at {:?}", label, mount_points);
                                let event = Event::new("disk", "mounted", {
                                    let mut m = HashMap::new();
                                    m.insert("label".to_string(), label);
                                    m.insert("device".to_string(), device);
                                    m.insert("mount_point".to_string(), mount_points[0].clone());
                                    m
                                });
                                let _ = self.event_tx.send(event).await;
                            }
                        }
                        MapTransition::Unchanged => {}
                        MapTransition::Updated { previous } => {
                            if !mount_points.is_empty() && previous.is_empty() {
                                let label = get_block_property(&ctx.system_bus, &path, "IdLabel").await;
                                let device = get_block_property(&ctx.system_bus, &path, "Device").await;
                                tracing::info!("Disk detector: mounted {} at {:?}", label, mount_points);
                                let event = Event::new("disk", "mounted", {
                                    let mut m = HashMap::new();
                                    m.insert("label".to_string(), label);
                                    m.insert("device".to_string(), device);
                                    m.insert("mount_point".to_string(), mount_points[0].clone());
                                    m
                                });
                                let _ = self.event_tx.send(event).await;
                            } else if mount_points.is_empty() && !previous.is_empty() {
                                let label = get_block_property(&ctx.system_bus, &path, "IdLabel").await;
                                let device = get_block_property(&ctx.system_bus, &path, "Device").await;
                                tracing::info!("Disk detector: unmounted {} from {:?}", label, previous);
                                let event = Event::new("disk", "unmounted", {
                                    let mut m = HashMap::new();
                                    m.insert("label".to_string(), label);
                                    m.insert("device".to_string(), device);
                                    m.insert("mount_point".to_string(), previous[0].clone());
                                    m
                                });
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

fn decode_mount_points(changed: &HashMap<String, zvariant::OwnedValue>) -> Vec<String> {
    let Some(value) = changed.get("MountPoints") else {
        return Vec::new();
    };
    
    let mut result = Vec::new();
    
    if let Ok(array) = value.downcast_ref::<zvariant::Array>() {
        for item in array.iter() {
            if let Ok(inner_array) = item.downcast_ref::<zvariant::Array>() {
                let mut bytes = Vec::new();
                for byte_item in inner_array.iter() {
                    if let Ok(b) = byte_item.downcast_ref::<u8>() {
                        bytes.push(b);
                    }
                }
                if let Ok(s) = std::str::from_utf8(&bytes) {
                    let trimmed = s.trim_end_matches('\0');
                    if !trimmed.is_empty() {
                        result.push(trimmed.to_string());
                    }
                }
            }
        }
    }
    
    result
}

async fn get_block_property(system_bus: &zbus::Connection, fs_path: &str, prop: &str) -> String {
    let block_path = fs_path.replace("/Filesystem", "/Block");
    
    let proxy = match zbus::Proxy::new(
        system_bus,
        UDISKS_BUS,
        block_path.as_str(),
        "org.freedesktop.DBus.Properties",
    ).await {
        Ok(p) => p,
        Err(_) => return String::new(),
    };

    match proxy.call_method("Get", &(UDISKS_BLOCK_IFACE, prop)).await {
        Ok(reply) => {
            let body = reply.body();
            if prop == "Device" {
                if let Ok(bytes) = body.deserialize::<Vec<u8>>() {
                    return String::from_utf8_lossy(&bytes).trim_end_matches('\0').to_string();
                }
            }
            body.deserialize::<String>().unwrap_or_default()
        }
        Err(_) => String::new(),
    }
}
