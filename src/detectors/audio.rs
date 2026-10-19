use crate::detectors::context::DetectorContext;
use crate::detectors::detector::Detector;
use crate::event::Event;
use parking_lot::Mutex;
use pulseaudio::protocol::{self, SubscriptionEvent, SubscriptionEventFacility, SubscriptionMask};
use std::collections::HashMap;
use std::ffi::CString;
use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::watch;
use tokio::time::{Duration, Instant};

/// Upper bound for one round of queries; the audio server may be hung or stopping.
const QUERY_TIMEOUT: Duration = Duration::from_secs(2);
/// One volume step emits a burst of change events (sink, source, and every
/// filter-chain node); wait for the burst to go quiet and refresh once.
const BURST_QUIET: Duration = Duration::from_millis(30);
const BURST_MAX: Duration = Duration::from_millis(150);
/// PA_VOLUME_NORM: the raw volume that means 100%.
const VOLUME_NORM: u64 = 0x10000;

#[derive(Debug, Clone, Default)]
struct AudioInfo {
    index: u32,
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
        // Both connections do a blocking handshake, so keep them off the runtime threads.
        let client = tokio::task::spawn_blocking(|| pulseaudio::Client::from_env(c"sysaud")).await??;
        let (mut events, _subscription) = tokio::task::spawn_blocking(Subscription::start).await??;

        tracing::info!("Audio detector: subscribed to PulseAudio events");
        self.initialize_defaults(&client).await?;

        loop {
            tokio::select! {
                event = events.recv() => {
                    let Some(event) = event else {
                        anyhow::bail!("PulseAudio subscription closed");
                    };
                    if self.is_relevant(&event) {
                        let closed = drain_burst(&mut events).await;
                        self.refresh_defaults(&client).await?;
                        if closed {
                            anyhow::bail!("PulseAudio subscription closed");
                        }
                    }
                }
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        tracing::info!("Audio detector: received shutdown signal");
                        break;
                    }
                }
            }
        }

        // Dropping `_subscription` shuts its socket down, which ends the reader thread.
        self.stop().await;
        Ok(())
    }

    async fn stop(&self) {}
}

/// The subscription lives on its own connection, read by a dedicated thread
/// because the protocol crate's message reader is blocking.
struct Subscription {
    socket: UnixStream,
}

impl Subscription {
    fn start() -> anyhow::Result<(mpsc::UnboundedReceiver<SubscriptionEvent>, Self)> {
        let path = pulseaudio::socket_path_from_env()
            .ok_or_else(|| anyhow::anyhow!("PulseAudio socket not found"))?;
        let socket = UnixStream::connect(path)?;
        // Bound the handshake; the read loop below then blocks indefinitely.
        socket.set_read_timeout(Some(QUERY_TIMEOUT))?;
        let mut reader = BufReader::new(socket.try_clone()?);
        let version = handshake(&mut reader)?;
        socket.set_read_timeout(None)?;

        let (tx, rx) = mpsc::unbounded_channel();
        std::thread::Builder::new()
            .name("sysaud-pulse-sub".into())
            .spawn(move || loop {
                match protocol::read_command_message(&mut reader, version) {
                    Ok((_, protocol::Command::SubscribeEvent(event))) => {
                        if tx.send(event).is_err() {
                            return;
                        }
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::debug!("Audio detector: subscription reader stopped: {}", e);
                        return;
                    }
                }
            })?;

        Ok((rx, Self { socket }))
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        let _ = self.socket.shutdown(std::net::Shutdown::Both);
    }
}

/// Auth, name the client, and subscribe to sink/source/server changes.
fn handshake(sock: &mut BufReader<UnixStream>) -> anyhow::Result<u16> {
    let cookie = pulseaudio::cookie_path_from_env()
        .and_then(|path| std::fs::read(path).ok())
        .unwrap_or_default();
    let auth = protocol::AuthParams {
        version: protocol::MAX_VERSION,
        supports_shm: false,
        supports_memfd: false,
        cookie,
    };
    protocol::write_command_message(sock.get_mut(), 0, &protocol::Command::Auth(auth), protocol::MAX_VERSION)?;
    let (_, reply) = protocol::read_reply_message::<protocol::AuthReply>(sock, protocol::MAX_VERSION)?;
    let version = std::cmp::min(protocol::MAX_VERSION, reply.version);

    let mut props = protocol::Props::new();
    props.set(protocol::Prop::ApplicationName, CString::new("sysaud-events").unwrap());
    protocol::write_command_message(sock.get_mut(), 1, &protocol::Command::SetClientName(props), version)?;
    let _ = protocol::read_reply_message::<protocol::SetClientNameReply>(sock, version)?;

    let mask = SubscriptionMask::SINK | SubscriptionMask::SOURCE | SubscriptionMask::SERVER;
    protocol::write_command_message(sock.get_mut(), 2, &protocol::Command::Subscribe(mask), version)?;
    protocol::read_ack_message(sock)?;
    Ok(version)
}

/// Swallow the rest of a burst of events. Returns true if the subscription closed.
async fn drain_burst(events: &mut mpsc::UnboundedReceiver<SubscriptionEvent>) -> bool {
    let deadline = Instant::now() + BURST_MAX;
    loop {
        let wait = BURST_QUIET.min(deadline.saturating_duration_since(Instant::now()));
        match tokio::time::timeout(wait, events.recv()).await {
            Ok(Some(_)) => continue,
            Ok(None) => return true,
            Err(_) => return false,
        }
    }
}

/// Server events can move the defaults; sink/source events only matter for the
/// current default device (not e.g. the hardware sink behind a filter chain).
fn is_relevant_event(event: &SubscriptionEvent, default_sink: Option<u32>, default_source: Option<u32>) -> bool {
    let is_default = |default: Option<u32>| default.is_none() || event.index.is_none() || event.index == default;
    match event.event_facility {
        SubscriptionEventFacility::Server => true,
        SubscriptionEventFacility::Sink => is_default(default_sink),
        SubscriptionEventFacility::Source => is_default(default_source),
        _ => false,
    }
}

impl AudioDetector {
    fn is_relevant(&self, event: &SubscriptionEvent) -> bool {
        let snapshot = self.snapshot.lock();
        is_relevant_event(
            event,
            snapshot.sink.as_ref().map(|s| s.index),
            snapshot.source.as_ref().map(|s| s.index),
        )
    }

    async fn initialize_defaults(&self, client: &pulseaudio::Client) -> anyhow::Result<()> {
        let (sink, source) = load_both(client).await?;
        let mut snapshot = self.snapshot.lock();
        snapshot.sink = sink;
        snapshot.source = source;
        Ok(())
    }

    async fn refresh_defaults(&self, client: &pulseaudio::Client) -> anyhow::Result<()> {
        let (sink, source) = load_both(client).await?;

        let mut events = Vec::new();
        {
            let mut snapshot = self.snapshot.lock();
            update_for_kind(&mut snapshot, AudioKind::Sink, sink, &mut events);
            update_for_kind(&mut snapshot, AudioKind::Source, source, &mut events);
        }

        for event in events {
            let _ = self.event_tx.send(event).await;
        }
        Ok(())
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

/// Default sink and source. Connection-level failures are returned as errors
/// so the supervisor reconnects; a device vanishing mid-query is just `None`.
async fn load_both(client: &pulseaudio::Client) -> anyhow::Result<(Option<AudioInfo>, Option<AudioInfo>)> {
    let query = async {
        let info = client.server_info().await?;
        let sink = async {
            match info.default_sink_name.clone() {
                Some(name) => device_or_none(client.sink_info_by_name(name).await.map(|s| {
                    audio_info(s.index, &s.name, s.description.as_deref(), &s.cvolume, s.muted)
                })),
                None => Ok(None),
            }
        };
        let source = async {
            match info.default_source_name.clone() {
                Some(name) => device_or_none(client.source_info_by_name(name).await.map(|s| {
                    audio_info(s.index, &s.name, s.description.as_deref(), &s.cvolume, s.muted)
                })),
                None => Ok(None),
            }
        };
        let (sink, source) = tokio::join!(sink, source);
        anyhow::Ok((sink?, source?))
    };

    tokio::time::timeout(QUERY_TIMEOUT, query)
        .await
        .map_err(|_| anyhow::anyhow!("PulseAudio queries timed out after {:?}", QUERY_TIMEOUT))?
}

fn device_or_none(result: Result<AudioInfo, pulseaudio::ClientError>) -> anyhow::Result<Option<AudioInfo>> {
    match result {
        Ok(info) => Ok(Some(info)),
        // e.g. the default device was removed between the two queries
        Err(pulseaudio::ClientError::ServerError(e)) => {
            tracing::debug!("Audio detector: device query failed: {:?}", e);
            Ok(None)
        }
        Err(e) => Err(e.into()),
    }
}

fn audio_info(
    index: u32,
    name: &std::ffi::CStr,
    description: Option<&std::ffi::CStr>,
    volume: &protocol::ChannelVolume,
    muted: bool,
) -> AudioInfo {
    AudioInfo {
        index,
        device: description.unwrap_or(name).to_string_lossy().into_owned(),
        volume: volume_percent(volume),
        muted,
    }
}

/// Per-channel percent rounded the way pactl prints it, then the integer mean.
fn volume_percent(volume: &protocol::ChannelVolume) -> Option<i32> {
    let channels = volume.channels();
    if channels.is_empty() {
        return None;
    }
    let sum: u64 = channels
        .iter()
        .map(|v| (v.as_u32() as u64 * 100 + VOLUME_NORM / 2) / VOLUME_NORM)
        .sum();
    Some((sum / channels.len() as u64) as i32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{ChannelVolume, SubscriptionEventType, Volume};

    fn volume(raw: &[u32]) -> ChannelVolume {
        let mut cv = ChannelVolume::empty();
        for &r in raw {
            cv.push(Volume::from_u32_clamped(r));
        }
        cv
    }

    #[test]
    fn volume_percent_matches_pactl_rounding() {
        // pactl showed 62259 as 95% and 58982 as 90%
        assert_eq!(volume_percent(&volume(&[62259, 62259])), Some(95));
        assert_eq!(volume_percent(&volume(&[62259, 58982])), Some(92));
        assert_eq!(volume_percent(&volume(&[0x10000])), Some(100));
        assert_eq!(volume_percent(&volume(&[])), None);
    }

    fn event(facility: SubscriptionEventFacility, index: u32) -> SubscriptionEvent {
        SubscriptionEvent {
            event_facility: facility,
            event_type: SubscriptionEventType::Changed,
            index: Some(index),
        }
    }

    #[test]
    fn only_default_devices_and_server_are_relevant() {
        let (sink, source) = (Some(36), Some(51));
        assert!(is_relevant_event(&event(SubscriptionEventFacility::Sink, 36), sink, source));
        assert!(!is_relevant_event(&event(SubscriptionEventFacility::Sink, 55), sink, source));
        assert!(is_relevant_event(&event(SubscriptionEventFacility::Source, 51), sink, source));
        assert!(!is_relevant_event(&event(SubscriptionEventFacility::Source, 36), sink, source));
        assert!(is_relevant_event(&event(SubscriptionEventFacility::Server, 0), sink, source));
        assert!(!is_relevant_event(&event(SubscriptionEventFacility::SinkInput, 36), sink, source));
        // before the first query we don't know the defaults yet
        assert!(is_relevant_event(&event(SubscriptionEventFacility::Sink, 55), None, None));
    }
}
