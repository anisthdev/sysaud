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

    let shared_config = config::SharedConfig::new(args.load_config()?);
    let sound_enabled = !args.no_sound;
    let notify_enabled = !args.no_notify;

    let session_bus = dbus::session_bus().await?;
    let system_bus = dbus::system_bus().await?;

    let (event_tx, mut event_rx) = mpsc::channel::<Event>(100);

    let sounds_config = config::sounds::SoundsConfig::default();
    let notifications_config = config::notifications::NotificationsConfig::default();

    let handlers = actions::build_handlers(actions::DispatcherConfig {
        config: shared_config.clone(),
        sounds_defaults: sounds_config,
        notifications_defaults: notifications_config,
        sound_enabled,
        notify_enabled,
        session_bus: session_bus.clone(),
    });

    let router = Arc::new(event::router::EventRouter::new(
        handlers,
        shared_config.snapshot().debounce.clone(),
    ));

    let registry = detectors::DetectorRegistry::new(event_tx);
    registry.attach_all(session_bus.clone(), system_bus.clone(), shared_config.clone()).await?;

    let event_loop_handle = tokio::spawn(async move {
        tracing::debug!("DEBUG: Event loop starting");
        while let Some(event) = event_rx.recv().await {
            tracing::debug!("DEBUG: Event loop received: {}", event.id);
            router.handle(&event);
        }
        tracing::debug!("DEBUG: Event loop receiver terminated (channel closed)");
    });

    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sigint = signal(SignalKind::interrupt())?;
    let mut sighup = signal(SignalKind::hangup())?;

    tracing::info!("sysaud running...");

    loop {
        tokio::select! {
            _ = sigterm.recv() => {
                tracing::info!("Received SIGTERM");
                break;
            }
            _ = sigint.recv() => {
                tracing::info!("Received SIGINT");
                break;
            }
            _ = sighup.recv() => {
                tracing::info!("Received SIGHUP - reloading config");
                match config::load(args.config.clone()) {
                    Ok(new_config) => {
                        shared_config.reload(new_config);
                        tracing::info!("Config reloaded successfully");
                    }
                    Err(e) => {
                        tracing::error!("Failed to reload config: {}", e);
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("Received ctrl-c");
                break;
            }
        }
    }

    tracing::info!("sysaud shutting down");
    tracing::debug!("DEBUG: Calling registry.shutdown().await");
    registry.shutdown().await;
    tracing::debug!("DEBUG: registry.shutdown().await completed");

    tracing::debug!("DEBUG: Dropping registry");
    drop(registry);
    tracing::debug!("DEBUG: registry dropped");

    tracing::debug!("DEBUG: Dropping session_bus and system_bus");
    drop(session_bus);
    drop(system_bus);
    tracing::debug!("DEBUG: Buses dropped");

    tracing::debug!("DEBUG: Awaiting event_loop_handle");
    if tokio::time::timeout(std::time::Duration::from_secs(2), event_loop_handle).await.is_err() {
        tracing::warn!("Event loop did not drain within 2s, exiting anyway");
    } else {
        tracing::debug!("DEBUG: event_loop_handle.await completed");
    }

    tracing::info!("sysaud shutdown complete");
    Ok(())
}
