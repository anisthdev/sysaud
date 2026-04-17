use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, watch};
use parking_lot::Mutex;
use libpulse_binding::context::{Context, FlagSet as ContextFlagSet, subscribe};
use libpulse_binding::mainloop::standard::Mainloop;
use libpulse_binding::context::introspect::Introspector;
use libpulse_binding::callbacks::ListResult;

use crate::event::Event;
use crate::detectors::context::DetectorContext;
use crate::detectors::detector::Detector;

#[derive(Clone)]
pub struct AudioDetector {
    event_tx: mpsc::Sender<Event>,
}

struct AudioState {
    sinks: HashMap<String, (f32, bool)>, // name -> (volume, mute)
    sources: HashMap<String, (f32, bool)>,
    default_sink: Option<String>,
    default_source: Option<String>,
}

impl Detector for AudioDetector {
    fn name(&self) -> &'static str {
        "audio"
    }

    fn new(event_tx: mpsc::Sender<Event>) -> Self {
        Self { event_tx }
    }

    async fn start(self: Arc<Self>, _ctx: DetectorContext, mut shutdown: watch::Receiver<bool>) -> anyhow::Result<()> {
        let event_tx = self.event_tx.clone();
        let mut shutdown_thread = shutdown.clone();
        
        // PulseAudio Mainloop is not Send, so we run it in a dedicated thread.
        let handle = std::thread::spawn(move || {
            let mut mainloop = Mainloop::new().expect("Failed to create PulseAudio mainloop");
            let mut context = Context::new(&mainloop, "sysaud-detector").expect("Failed to create PulseAudio context");

            context.connect(None, ContextFlagSet::NOFLAGS, None).expect("Failed to connect PulseAudio context");

            // Wait for context to be ready (blocking in this thread)
            loop {
                mainloop.iterate(false);
                match context.get_state() {
                    libpulse_binding::context::State::Ready => break,
                    libpulse_binding::context::State::Failed | libpulse_binding::context::State::Terminated => {
                        return Err(anyhow::anyhow!("PulseAudio context failed or terminated"));
                    }
                    _ => {}
                }
            }

            let state = Arc::new(Mutex::new(AudioState {
                sinks: HashMap::new(),
                sources: HashMap::new(),
                default_sink: None,
                default_source: None,
            }));

            // Subscribe to events
            context.subscribe(
                subscribe::InterestMaskSet::SINK | subscribe::InterestMaskSet::SOURCE | subscribe::InterestMaskSet::SERVER,
                |_| {}
            );

            let mut last_check = std::time::Instant::now();

            loop {
                mainloop.iterate(false);
                
                if last_check.elapsed() >= std::time::Duration::from_millis(500) {
                    let introspect = context.introspect();
                    check_audio_state(&introspect, &state, &event_tx);
                    last_check = std::time::Instant::now();
                }

                // Check shutdown without blocking
                if shutdown_thread.has_changed().unwrap_or(false) {
                    if *shutdown_thread.borrow_and_update() {
                        tracing::info!("Audio detector: received shutdown signal in thread");
                        break;
                    }
                }

                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Ok(())
        });

        loop {
            tokio::select! {
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        break;
                    }
                }
            }
        }
        
        let _ = tokio::task::spawn_blocking(move || handle.join()).await;

        self.stop().await;
        Ok(())
    }

    async fn stop(&self) {}
}

fn check_audio_state(
    introspect: &Introspector,
    state_mu: &Arc<Mutex<AudioState>>,
    event_tx: &mpsc::Sender<Event>,
) {
    let state_sink = state_mu.clone();
    let tx_sink = event_tx.clone();
    introspect.get_sink_info_list(move |result| {
        if let ListResult::Item(item) = result {
            let mut state = state_sink.lock();
            let name = item.name.as_ref().map(|s| s.to_string()).unwrap_or_default();
            let volume = item.volume.avg().0 as f32 / 65536.0;
            let mute = item.mute;

            let prev = state.sinks.get(&name).cloned();
            if let Some((prev_vol, prev_mute)) = prev {
                if (volume - prev_vol).abs() > 0.01 {
                    let vol_pct = (volume * 100.0).round() as i32;
                    tracing::info!("Audio detector: Sink {} volume changed to {}%", name, vol_pct);
                    let mut details = HashMap::new();
                    details.insert("name".to_string(), name.clone());
                    details.insert("volume".to_string(), vol_pct.to_string());
                    let _ = tx_sink.try_send(Event::new("audio", "volume-changed", details));
                }
                if mute != prev_mute {
                    tracing::info!("Audio detector: Sink {} mute toggled to {}", name, mute);
                    let mut details = HashMap::new();
                    details.insert("name".to_string(), name.clone());
                    details.insert("mute".to_string(), mute.to_string());
                    let _ = tx_sink.try_send(Event::new("audio", "mute-toggled", details));
                }
            }
            state.sinks.insert(name, (volume, mute));
        }
    });

    let state_source = state_mu.clone();
    let tx_source = event_tx.clone();
    introspect.get_source_info_list(move |result| {
        if let ListResult::Item(item) = result {
            let mut state = state_source.lock();
            let name = item.name.as_ref().map(|s| s.to_string()).unwrap_or_default();
            let volume = item.volume.avg().0 as f32 / 65536.0;
            let mute = item.mute;

            let prev = state.sources.get(&name).cloned();
            if let Some((prev_vol, prev_mute)) = prev {
                if (volume - prev_vol).abs() > 0.01 {
                    let vol_pct = (volume * 100.0).round() as i32;
                    tracing::info!("Audio detector: Source {} volume changed to {}%", name, vol_pct);
                    let mut details = HashMap::new();
                    details.insert("name".to_string(), name.clone());
                    details.insert("volume".to_string(), vol_pct.to_string());
                    let _ = tx_source.try_send(Event::new("audio", "source-volume-changed", details));
                }
                if mute != prev_mute {
                    tracing::info!("Audio detector: Source {} mute toggled to {}", name, mute);
                    let mut details = HashMap::new();
                    details.insert("name".to_string(), name.clone());
                    details.insert("mute".to_string(), mute.to_string());
                    let _ = tx_source.try_send(Event::new("audio", "source-mute-toggled", details));
                }
            }
            state.sources.insert(name, (volume, mute));
        }
    });

    let state_server = state_mu.clone();
    let tx_server = event_tx.clone();
    introspect.get_server_info(move |result| {
        let mut state = state_server.lock();
        let sink = result.default_sink_name.as_ref().map(|s| s.to_string());
        let source = result.default_source_name.as_ref().map(|s| s.to_string());

        if sink != state.default_sink && sink.is_some() {
            let name = sink.clone().unwrap();
            tracing::info!("Audio detector: Default sink changed to {}", name);
            let mut details = HashMap::new();
            details.insert("name".to_string(), name);
            let _ = tx_server.try_send(Event::new("audio", "sink-changed", details));
        }
        if source != state.default_source && source.is_some() {
            let name = source.clone().unwrap();
            tracing::info!("Audio detector: Default source changed to {}", name);
            let mut details = HashMap::new();
            details.insert("name".to_string(), name);
            let _ = tx_server.try_send(Event::new("audio", "source-changed", details));
        }

        state.default_sink = sink;
        state.default_source = source;
    });
}
