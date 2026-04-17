use crate::config::sounds::SoundsConfig;
use crate::config::notifications::NotificationsConfig;
use crate::event::handler::EventHandler;
use std::sync::Arc;
use zbus::Connection;

pub mod logger;
pub mod notify;
pub mod script;
pub mod sound;

pub struct ActionDispatcher {
    _phantom: std::marker::PhantomData<()>,
}

pub struct DispatcherConfig {
    pub config: Arc<crate::config::Config>,
    pub sounds_defaults: SoundsConfig,
    pub notifications_defaults: NotificationsConfig,
    pub sound_enabled: bool,
    pub notify_enabled: bool,
    pub session_bus: Connection,
}

impl ActionDispatcher {
    pub fn new(config: DispatcherConfig) -> (Vec<Box<dyn EventHandler>>, Self) {
        let mut handlers: Vec<Box<dyn EventHandler>> = Vec::new();

        handlers.push(Box::new(logger::LoggerHandler::new(
            config.config.clone(),
        )));

        handlers.push(Box::new(sound::SoundHandler::new(
            config.config.clone(),
            config.sounds_defaults,
            config.sound_enabled,
        )));

        handlers.push(Box::new(notify::NotifyHandler::new(
            config.config.clone(),
            config.notifications_defaults,
            config.notify_enabled,
            config.session_bus,
        )));

        handlers.push(Box::new(script::ScriptHandler::new(
            config.config.clone(),
        )));

        (handlers, Self { _phantom: std::marker::PhantomData })
    }
}
