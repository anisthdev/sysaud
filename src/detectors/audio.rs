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

    fn default_label(self) -> &'static str {
        match self {
            Self::Sink => "Default Sink",
            Self::Source => "Default Source",
        }
    }

    fn list_kind(self) -> &'static str {
        match self {
            Self::Sink => "sinks",
            Self::Source => "sources",
        }
    }

    fn volume_command(self) -> &'static str {
        match self {
            Self::Sink => "get-sink-volume",
            Self::Source => "get-source-volume",
        }
    }

    fn mute_command(self) -> &'static str {
        match self {
            Self::Sink => "get-sink-mute",
            Self::Source => "get-source-mute",
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
                                self.refresh_defaults().await;
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
        .spawn()?;

    Ok(child)
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
        let sink = load_audio_info(AudioKind::Sink).await;
        let source = load_audio_info(AudioKind::Source).await;
        let mut snapshot = self.snapshot.lock();
        snapshot.sink = sink;
        snapshot.source = source;
    }

    async fn refresh_defaults(&self) {
        let sink = load_audio_info(AudioKind::Sink).await;
        let source = load_audio_info(AudioKind::Source).await;

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

async fn load_audio_info(kind: AudioKind) -> Option<AudioInfo> {
    let default_name = read_default_name(kind).await?;
    let device = friendly_name(kind, &default_name).await.unwrap_or(default_name.clone());
    let muted = read_mute(kind, &default_name).await.unwrap_or(false);
    let volume = read_volume(kind, &default_name).await.unwrap_or(None);

    Some(AudioInfo {
        device,
        volume,
        muted,
    })
}

async fn read_default_name(kind: AudioKind) -> Option<String> {
    let output = Command::new("pactl")
        .arg("info")
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let label = kind.default_label();

    for line in stdout.lines() {
        let line = line.trim();
        if let Some(value) = line.strip_prefix(label) {
            return Some(value.trim().trim_start_matches(':').trim().to_string());
        }
    }

    None
}

async fn read_mute(kind: AudioKind, target: &str) -> anyhow::Result<bool> {
    let output = Command::new("pactl")
        .arg(kind.mute_command())
        .arg(target)
        .output()
        .await?;

    if !output.status.success() {
        return Ok(false);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let line = line.trim();
        if let Some(value) = line.strip_prefix("Mute:") {
            return Ok(value.trim() == "yes");
        }
    }

    Ok(false)
}

async fn read_volume(kind: AudioKind, target: &str) -> anyhow::Result<Option<i32>> {
    let output = Command::new("pactl")
        .arg(kind.volume_command())
        .arg(target)
        .output()
        .await?;

    if !output.status.success() {
        return Ok(None);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_volume_percent(&stdout))
}

async fn friendly_name(kind: AudioKind, target: &str) -> Option<String> {
    let output = Command::new("pactl")
        .arg("list")
        .arg(kind.list_kind())
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut current_name: Option<String> = None;

    for line in stdout.lines() {
        let line = line.trim();
        if let Some(name) = line.strip_prefix("Name:") {
            current_name = Some(name.trim().to_string());
            continue;
        }

        if let Some(desc) = line.strip_prefix("Description:") {
            if current_name.as_deref() == Some(target) {
                return Some(desc.trim().to_string());
            }
        }
    }

    None
}

fn parse_volume_percent(output: &str) -> Option<i32> {
    let mut values = Vec::new();

    for part in output.split('/') {
        let trimmed = part.trim();
        let Some(percent) = trimmed.strip_suffix('%') else {
            continue;
        };

        if let Ok(value) = percent.trim().parse::<f32>() {
            values.push(value.round() as i32);
        }
    }

    if values.is_empty() {
        None
    } else {
        Some(values.iter().sum::<i32>() / values.len() as i32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_volume_percentages() {
        let output = "Volume: front-left: 65536 / 100% / 0.00 dB, front-right: 65536 / 100% / 0.00 dB";
        assert_eq!(parse_volume_percent(output), Some(100));
    }

    #[test]
    fn recognizes_audio_lines() {
        assert!(is_audio_relevant_line("Event 'change' on sink #42"));
        assert!(is_audio_relevant_line("Event 'change' on source #7"));
        assert!(is_audio_relevant_line("Event 'change' on server #1"));
        assert!(!is_audio_relevant_line("Event 'new' on client #10"));
    }
}
