use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::watch;
use futures_util::StreamExt;
use crate::event::Event;
use crate::dbus;
use crate::detectors::context::DetectorContext;
use crate::detectors::detector::Detector;
use crate::detectors::state::{StateCell, StateTransition};

#[derive(Clone)]
pub struct SessionDetector {
    event_tx: mpsc::Sender<Event>,
    locked: StateCell<bool>,
}

impl Detector for SessionDetector {
    fn name(&self) -> &'static str {
        "session"
    }

    fn new(event_tx: mpsc::Sender<Event>) -> Self {
        Self { event_tx, locked: StateCell::new() }
    }

    async fn start(self: Arc<Self>, ctx: DetectorContext, mut shutdown: watch::Receiver<bool>) -> anyhow::Result<()> {
        let config = ctx.config.clone();

        let self_for_poll = self.clone();
        let mut shutdown_poll = shutdown.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(500));
            loop {
                tokio::select! {
                    _ = interval.tick() => {}
                    _ = shutdown_poll.changed() => {
                        if *shutdown_poll.borrow() {
                            tracing::debug!("DEBUG: Session poll task exiting");
                            break;
                        }
                    }
                }
                let lock_app = config.snapshot().daemon.lock_app.clone();
                let is_running = {
                    let lock_app = lock_app.clone();
                    tokio::task::spawn_blocking(move || Self::is_process_running(&lock_app))
                        .await
                        .unwrap_or(false)
                };

                match self_for_poll.locked.update(is_running).await {
                    StateTransition::Initialized | StateTransition::Unchanged => {}
                    StateTransition::Changed { previous } => {
                        if previous && !is_running {
                            tracing::info!("Session detector: {} exited, emitting screen-unlocked", lock_app);
                            let event = Event::new("session", "screen-unlocked", HashMap::new());
                            let _ = self_for_poll.event_tx.send(event).await;
                        } else if !previous && is_running {
                            tracing::info!("Session detector: {} started, emitting screen-locked", lock_app);
                            let event = Event::new("session", "screen-locked", HashMap::new());
                            let _ = self_for_poll.event_tx.send(event).await;
                        }
                    }
                }
            }
        });

        let self_for_ss = self.clone();
        let session_bus = ctx.session_bus.clone();
        let mut shutdown_ss = shutdown.clone();
        tokio::spawn(async move {
            let rule = zbus::MatchRule::builder()
                .interface("org.freedesktop.ScreenSaver")?
                .member("ActiveChanged")?
                .build();

            let mut stream = dbus::matched_stream(&session_bus, vec![rule.clone()]).await?;

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

                        if let Ok(is_locked) = msg.body().deserialize::<bool>() {
                            match self_for_ss.locked.update(is_locked).await {
                                StateTransition::Initialized | StateTransition::Unchanged => {}
                                StateTransition::Changed { .. } => {
                                    tracing::info!("Session detector: ScreenSaver ActiveChanged to {}, emitting event", is_locked);
                                    let name = if is_locked { "screen-locked" } else { "screen-unlocked" };
                                    let event = Event::new("session", name, HashMap::new());
                                    let _ = self_for_ss.event_tx.send(event).await;
                                }
                            }
                        }
                    }
                    _ = shutdown_ss.changed() => {
                        if *shutdown_ss.borrow() {
                            tracing::debug!("DEBUG: Session ScreenSaver task exiting");
                            break;
                        }
                    }
                }
            }
            Ok::<(), anyhow::Error>(())
        });

        let prop_rule = zbus::MatchRule::builder()
            .interface("org.freedesktop.DBus.Properties")?
            .member("PropertiesChanged")?
            .path_namespace("/org/freedesktop/login1/session")?
            .build();

        let lock_rule = zbus::MatchRule::builder()
            .interface("org.freedesktop.login1.Session")?
            .path_namespace("/org/freedesktop/login1/session")?
            .build();
        let mut stream = dbus::matched_stream(&ctx.system_bus, vec![prop_rule.clone(), lock_rule.clone()]).await?;

        loop {
            tokio::select! {
                msg = stream.next() => {
                    let msg = match msg {
                        Some(Ok(m)) => m,
                        _ => break,
                    };

                    if lock_rule.matches(&msg).unwrap_or(false) {
                        if let Some(member) = msg.header().member() {
                            let is_locked = match member.as_str() {
                                "Lock" => Some(true),
                                "Unlock" => Some(false),
                                _ => None,
                            };

                            if let Some(is_locked) = is_locked {
                                match self.locked.update(is_locked).await {
                                    StateTransition::Initialized | StateTransition::Unchanged => {}
                                    StateTransition::Changed { .. } => {
                                        let name = if is_locked { "screen-locked" } else { "screen-unlocked" };
                                        tracing::info!("Session detector: received logind signal {}, emitting event", name);
                                        let event = Event::new("session", name, HashMap::new());
                                        let _ = self.event_tx.send(event).await;
                                    }
                                }
                            }
                        }
                    }

                    if prop_rule.matches(&msg).unwrap_or(false) {
                        if let Ok((interface, changed, _)) = msg.body().deserialize::<(String, HashMap<String, zvariant::OwnedValue>, Vec<String>)>() {
                            if interface == "org.freedesktop.login1.Session" {
                                if let Some(value) = changed.get("LockedHint") {
                                    if let Ok(is_locked) = value.downcast_ref::<bool>() {
                                        match self.locked.update(is_locked).await {
                                            StateTransition::Initialized | StateTransition::Unchanged => {}
                                            StateTransition::Changed { .. } => {
                                                tracing::info!("Session detector: logind LockedHint changed to {}, emitting event", is_locked);
                                                let name = if is_locked { "screen-locked" } else { "screen-unlocked" };
                                                let event = Event::new("session", name, HashMap::new());
                                                let _ = self.event_tx.send(event).await;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        tracing::info!("Session detector: received shutdown signal");
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

impl SessionDetector {
    /// Same match as `pgrep -x NAME`: some process's comm equals `name` exactly.
    /// Reads /proc directly instead of forking pgrep on every poll.
    fn is_process_running(name: &str) -> bool {
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return false;
        };

        entries
            .flatten()
            .filter(|entry| entry.file_name().to_str().is_some_and(|n| n.bytes().all(|b| b.is_ascii_digit())))
            .any(|entry| {
                std::fs::read_to_string(entry.path().join("comm"))
                    .is_ok_and(|comm| comm.trim_end_matches('\n') == name)
            })
    }
}

#[derive(Clone)]
pub struct SuspendDetector {
    event_tx: mpsc::Sender<Event>,
}

impl Detector for SuspendDetector {
    fn name(&self) -> &'static str {
        "suspend"
    }

    fn new(event_tx: mpsc::Sender<Event>) -> Self {
        Self { event_tx }
    }

    async fn start(self: Arc<Self>, ctx: DetectorContext, mut shutdown: watch::Receiver<bool>) -> anyhow::Result<()> {
        let rule = zbus::MatchRule::builder()
            .interface("org.freedesktop.login1.Manager")?
            .member("PrepareForSleep")?
            .path("/org/freedesktop/login1")?
            .build();
        dbus::register_match_rules(&ctx.system_bus, vec![rule]).await?;

        let proxy: zbus::Proxy = zbus::Proxy::new(
            &ctx.system_bus,
            "org.freedesktop.login1",
            "/org/freedesktop/login1",
            "org.freedesktop.login1.Manager",
        ).await?;

        let mut stream = proxy.receive_signal("PrepareForSleep").await?;

        tracing::info!("Suspend detector: subscribed to logind PrepareForSleep");

        loop {
            tokio::select! {
                biased;

                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        tracing::info!("Suspend detector: received shutdown signal");
                        break;
                    }
                }
                signal = stream.next() => {
                    let signal = match signal {
                        Some(s) => s,
                        None => break,
                    };
                    let going_to_sleep: bool = match signal.body().deserialize() {
                        Ok(b) => b,
                        Err(_) => continue,
                    };

                    let name = if going_to_sleep { "suspend" } else { "resume" };
                    tracing::info!("Suspend detector: emitting event session.{}", name);
                    let event = Event::new("session", name, HashMap::new());
                    let _ = self.event_tx.send(event).await;
                }
            }
        }

        self.stop().await;
        Ok(())
    }

    async fn stop(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_running_process_by_exact_comm() {
        let own = std::fs::read_to_string("/proc/self/comm").unwrap();
        let own = own.trim_end_matches('\n');
        assert!(SessionDetector::is_process_running(own));
        assert!(!SessionDetector::is_process_running(&own[..own.len() - 1]));
        assert!(!SessionDetector::is_process_running("sysaud-no-such-process"));
    }
}
