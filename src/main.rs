mod actions;
mod cli;
mod config;
mod dbus;
mod detectors;
pub mod event;
mod pidlock;

use clap::Parser;
use cli::{Args, Command};
use event::Event;
use event::router::EventRouter;
use std::sync::Arc;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(
                    if args.verbose {
                        tracing::Level::DEBUG.into()
                    } else {
                        tracing::Level::INFO.into()
                    }
                )
        )
        .init();

    match &args.command {
        Some(Command::Check) => {
            match config::load(args.config.clone()) {
                Ok(cfg) => {
                    println!("Config is valid");
                    println!("  daemon.lock_app: {}", cfg.daemon.lock_app);
                    println!("  sound.enabled: {}", cfg.sound.enabled);
                    println!("  notify.enabled: {}", cfg.notify.enabled);
                    println!("  events: {} configured", cfg.events.len());
                    Ok(())
                }
                Err(e) => {
                    eprintln!("Config error: {e}");
                    Err(e)
                }
            }?;
        }
        Some(Command::ListEvents) => {
            let events = cli::list_events();
            if args.json {
                let json = serde_json::to_string_pretty(&events)?;
                println!("{json}");
            } else {
                println!("Supported events:");
                for event in &events {
                    println!("  {event}");
                }
                println!("\n{} events total", events.len());
            }
        }
        None => {
            run_daemon(args).await?;
        }
    }

    Ok(())
}

async fn run_daemon(args: Args) -> anyhow::Result<()> {
    tracing::info!("sysaud starting...");

    let _pid_lock = pidlock::PidLock::acquire()?;

    let initial_config = args.load_config()?;
    let sound_enabled = !args.no_sound;
    let notify_enabled = !args.no_notify;

    let session_bus = dbus::session_bus().await?;
    let system_bus = dbus::system_bus().await?;

    let (event_tx, mut event_rx) = mpsc::channel::<Event>(100);

    let sounds_config = config::sounds::SoundsConfig::default();
    let notifications_config = config::notifications::NotificationsConfig::default();

    let shared_config = config::SharedConfig::new(initial_config);

    let (handlers, dispatcher) = actions::ActionDispatcher::new(actions::DispatcherConfig {
        config: shared_config.get_config(),
        sounds_defaults: sounds_config,
        notifications_defaults: notifications_config,
        sound_enabled,
        notify_enabled,
        session_bus: session_bus.clone(),
    });

    let _ = dispatcher;
    let router = Arc::new(EventRouter::new(handlers, shared_config.get_config().debounce.clone()));

    let registry = detectors::DetectorRegistry::new(event_tx);
    registry.attach_all(session_bus, system_bus, shared_config.get_config()).await?;

    tokio::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            router.handle(&event);
        }
    });

    setup_signal_handlers(registry.clone(), shared_config.clone(), args.config.clone()).await?;

    tracing::info!("sysaud running...");
    tokio::signal::ctrl_c().await?;
    tracing::info!("sysaud shutting down");
    registry.shutdown().await;
    Ok(())
}

async fn setup_signal_handlers(
    registry: detectors::DetectorRegistry,
    shared_config: config::SharedConfig,
    config_path: Option<std::path::PathBuf>,
) -> anyhow::Result<()> {
    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sigint = signal(SignalKind::interrupt())?;
    let mut sighup = signal(SignalKind::hangup())?;

    tokio::spawn(async move {
        tokio::select! {
            _ = sigterm.recv() => {
                tracing::info!("Received SIGTERM");
                registry.shutdown().await;
            }
            _ = sigint.recv() => {
                tracing::info!("Received SIGINT");
                registry.shutdown().await;
            }
            _ = sighup.recv() => {
                tracing::info!("Received SIGHUP - reloading config");
                match config::load(config_path) {
                    Ok(new_config) => {
                        shared_config.reload(new_config);
                        tracing::info!("Config reloaded successfully");
                    }
                    Err(e) => {
                        tracing::error!("Failed to reload config: {}", e);
                    }
                }
            }
        }
    });

    Ok(())
}
