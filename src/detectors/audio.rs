use crate::detectors::context::DetectorContext;
use crate::detectors::detector::Detector;
use crate::event::Event;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::sync::watch;
use tokio::time::{Duration, Instant};

/// Upper bound for one-shot pactl queries; the audio server may be hung or stopping.
const PACTL_TIMEOUT: Duration = Duration::from_secs(2);
/// One volume step emits a burst of change lines (sink, source, and every
/// filter-chain node); wait for the burst to go quiet and refresh once.
const BURST_QUIET: Duration = Duration::from_millis(30);
const BURST_MAX: Duration = Duration::from_millis(150);

#[derive(Debug, Clone, Default)]
struct AudioInfo {
    device: String,
    volume: Option<i32>,
    muted: bool,
}

#[derive(Debug, Clone, Default)]
struct AudioSnapshot {
    sink: Option<AudioInfo>,
    source: Option<AudioInfo>,
}

#[derive(Clone)]
pub struct AudioDetector {
    event_tx: mpsc::Sender<Event>,
    snapshot: Arc<Mutex<AudioSnapshot>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AudioKind {
    Sink,
    Source,
}

impl AudioKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Sink => "sink",
            Self::Source => "source",
        }
    }

    fn list_kind(self) -> &'static str {
        match self {
            Self::Sink => "sinks",
            Self::Source => "sources",
        }
    }

    fn default_change_event(self) -> &'static str {
        match self {
            Self::Sink => "default-sink-changed",
            Self::Source => "default-source-changed",
        }
    }
}

impl Detector for AudioDetector {
    fn name(&self) -> &'static str {
        "audio"
    }

    fn new(event_tx: mpsc::Sender<Event>) -> Self {
        Self {
            event_tx,
            snapshot: Arc::new(Mutex::new(AudioSnapshot::default())),
        }
    }

    async fn start(self: Arc<Self>, _ctx: DetectorContext, mut shutdown: watch::Receiver<bool>) -> anyhow::Result<()> {
        let mut child = start_pactl_subscribe().await?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("pactl subscribe did not provide stdout"))?;
        let mut lines = BufReader::new(stdout).lines();

        tracing::info!("Audio detector: subscribed to pactl events");
        self.initialize_defaults().await;

        loop {
            tokio::select! {
                line = lines.next_line() => {
                    match line? {
                        Some(line) => {
                            if is_audio_relevant_line(&line) {
                                let eof = drain_burst(&mut lines).await;
                                self.refresh_defaults().await;
                                if eof {
                                    break;
                                }
                            } else if !is_client_line(&line) {
                                tracing::trace!("Audio detector: ignored pactl line: {}", line);
                            }
                        }
                        None => {
                            if let Some(code) = child.wait().await.ok().and_then(|status| status.code()) {
                                tracing::warn!("Audio detector: pactl subscribe exited with code {}", code);
                            }
                            break;
                        }
                    }
                }
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        tracing::info!("Audio detector: received shutdown signal");
                        let _ = child.kill().await;
                        let _ = child.wait().await;
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

async fn start_pactl_subscribe() -> anyhow::Result<Child> {
    let child = Command::new("pactl")
        .arg("subscribe")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()?;

    Ok(child)
}

/// Swallow the rest of a burst of pactl events. Returns true if pactl exited.
async fn drain_burst<R: tokio::io::AsyncBufRead + Unpin>(lines: &mut tokio::io::Lines<R>) -> bool {
    let deadline = Instant::now() + BURST_MAX;
    loop {
        let wait = BURST_QUIET.min(deadline.saturating_duration_since(Instant::now()));
        match tokio::time::timeout(wait, lines.next_line()).await {
            Ok(Ok(Some(_))) => continue,
            Ok(Ok(None)) | Ok(Err(_)) => return true,
            Err(_) => return false,
        }
    }
}

fn is_client_line(line: &str) -> bool {
    let line = line.trim();
    line.starts_with("Event 'new' on client #") || line.starts_with("Event 'remove' on client #")
}

fn is_audio_relevant_line(line: &str) -> bool {
    let line = line.trim();
    let Some(rest) = line.strip_prefix("Event '") else {
        return false;
    };
    let Some((_action, rest)) = rest.split_once("' on ") else {
        return false;
    };
    let Some((kind, index)) = rest.rsplit_once(" #") else {
        return false;
    };

    matches!(kind, "sink" | "source" | "server") && index.parse::<u32>().is_ok()
}

impl AudioDetector {
    async fn initialize_defaults(&self) {
        let (sink, source) = load_both().await;
        let mut snapshot = self.snapshot.lock();
        snapshot.sink = sink;
        snapshot.source = source;
    }

    async fn refresh_defaults(&self) {
        let (sink, source) = load_both().await;

        let mut events = Vec::new();
        {
            let mut snapshot = self.snapshot.lock();
            update_for_kind(&mut snapshot, AudioKind::Sink, sink, &mut events);
            update_for_kind(&mut snapshot, AudioKind::Source, source, &mut events);
        }

        for event in events {
            let _ = self.event_tx.send(event).await;
        }
    }
}

fn update_for_kind(
    snapshot: &mut AudioSnapshot,
    kind: AudioKind,
    current: Option<AudioInfo>,
    events: &mut Vec<Event>,
) {
    let Some(info) = current else {
        return;
    };

    let slot = match kind {
        AudioKind::Sink => &mut snapshot.sink,
        AudioKind::Source => &mut snapshot.source,
    };

    let previous = slot.clone();
    if let Some(prev) = previous {
        if prev.device != info.device {
            events.push(make_event("audio".to_string(), kind.default_change_event(), HashMap::from([
                ("device".to_string(), info.device.clone()),
                ("type".to_string(), kind.as_str().to_string()),
            ])));
        }

        if prev.muted != info.muted {
            events.push(make_event("audio".to_string(), "mute-changed", HashMap::from([
                ("device".to_string(), info.device.clone()),
                ("muted".to_string(), info.muted.to_string()),
                ("type".to_string(), kind.as_str().to_string()),
            ])));
        }

        if !info.muted && prev.volume != info.volume {
            if let Some(volume) = info.volume {
                events.push(make_event("audio".to_string(), "volume-changed", HashMap::from([
                    ("device".to_string(), info.device.clone()),
                    ("type".to_string(), kind.as_str().to_string()),
                    ("volume".to_string(), volume.to_string()),
                ])));
            }
        }
    }

    *slot = Some(info);
}

fn make_event(category: String, name: &str, detail: HashMap<String, String>) -> Event {
    Event::new(category, name.to_string(), detail)
}

async fn pactl(args: &[&str]) -> anyhow::Result<std::process::Output> {
    let output = Command::new("pactl")
        .args(args)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();

    tokio::time::timeout(PACTL_TIMEOUT, output)
        .await
        .map_err(|_| anyhow::anyhow!("pactl {} timed out after {:?}", args.join(" "), PACTL_TIMEOUT))?
        .map_err(Into::into)
}

/// Default sink and source, from one `pactl info`.
async fn load_both() -> (Option<AudioInfo>, Option<AudioInfo>) {
    let info = match pactl(&["-f", "json", "info"]).await {
        Ok(output) if output.status.success() => output.stdout,
        Ok(_) => return (None, None),
        Err(e) => {
            tracing::debug!("Audio detector: {}", e);
            return (None, None);
        }
    };
    let Ok(info) = serde_json::from_slice::<serde_json::Value>(&info) else {
        return (None, None);
    };

    let sink = info["default_sink_name"].as_str().map(str::to_owned);
    let source = info["default_source_name"].as_str().map(str::to_owned);
    tokio::join!(
        load_audio_info(AudioKind::Sink, sink),
        load_audio_info(AudioKind::Source, source),
    )
}

async fn load_audio_info(kind: AudioKind, default_name: Option<String>) -> Option<AudioInfo> {
    let default_name = default_name?;
    let devices = pactl(&["-f", "json", "list", kind.list_kind()])
        .await
        .ok()
        .filter(|output| output.status.success())
        .map(|output| output.stdout)
        .unwrap_or_default();

    Some(parse_device_info(&devices, &default_name))
}

/// Look `target` up in `pactl -f json list sinks|sources` output. If it can't be
/// found, fall back to the raw name with unknown volume, as before.
fn parse_device_info(devices: &[u8], target: &str) -> AudioInfo {
    let devices: Vec<serde_json::Value> = serde_json::from_slice(devices).unwrap_or_default();
    let Some(dev) = devices.iter().find(|d| d["name"].as_str() == Some(target)) else {
        return AudioInfo {
            device: target.to_string(),
            volume: None,
            muted: false,
        };
    };

    AudioInfo {
        device: dev["description"].as_str().unwrap_or(target).to_string(),
        volume: parse_volume_percent(&dev["volume"]),
        muted: dev["mute"].as_bool().unwrap_or(false),
    }
}

/// Average of the per-channel `value_percent` entries, e.g. {"front-left": {"value_percent": "65%"}}.
fn parse_volume_percent(volume: &serde_json::Value) -> Option<i32> {
    let values: Vec<i32> = volume
        .as_object()?
        .values()
        .filter_map(|channel| channel["value_percent"].as_str())
        .filter_map(|p| p.trim().strip_suffix('%')?.trim().parse::<f32>().ok())
        .map(|v| v.round() as i32)
        .collect();

    if values.is_empty() {
        None
    } else {
        Some(values.iter().sum::<i32>() / values.len() as i32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SINKS: &str = r#"[
        {"name":"effect_input.laptop_warm","description":"Laptop","mute":false,
         "volume":{"front-left":{"value":62259,"value_percent":"95%","db":"-1.34 dB"},
                   "front-right":{"value":58982,"value_percent":"90%","db":"-2.75 dB"}}},
        {"name":"alsa_output.raw","description":"Laptop (Raw)","mute":true,
         "volume":{"aux0":{"value":65536,"value_percent":"100%","db":"0.00 dB"}}}
    ]"#;

    #[test]
    fn parses_device_from_json_list() {
        let info = parse_device_info(SINKS.as_bytes(), "effect_input.laptop_warm");
        assert_eq!(info.device, "Laptop");
        assert_eq!(info.volume, Some(92)); // integer mean of 95 and 90, same as the old text parser
        assert!(!info.muted);

        let raw = parse_device_info(SINKS.as_bytes(), "alsa_output.raw");
        assert_eq!(raw.device, "Laptop (Raw)");
        assert_eq!(raw.volume, Some(100));
        assert!(raw.muted);
    }

    #[test]
    fn unknown_device_falls_back_to_raw_name() {
        let info = parse_device_info(SINKS.as_bytes(), "missing.sink");
        assert_eq!(info.device, "missing.sink");
        assert_eq!(info.volume, None);
        assert!(!info.muted);
        assert_eq!(parse_device_info(b"not json", "x").device, "x");
    }

    #[test]
    fn recognizes_audio_lines() {
        assert!(is_audio_relevant_line("Event 'change' on sink #42"));
        assert!(is_audio_relevant_line("Event 'change' on source #7"));
        assert!(is_audio_relevant_line("Event 'change' on server #1"));
        assert!(!is_audio_relevant_line("Event 'new' on client #10"));
    }
}
