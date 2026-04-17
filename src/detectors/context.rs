use crate::config::Config;
use crate::event::Event;
use std::sync::Arc;
use zbus::Connection;

#[derive(Clone)]
pub struct DetectorContext {
    pub event_tx: tokio::sync::mpsc::Sender<Event>,
    pub config: Arc<Config>,
    pub session_bus: Connection,
    pub system_bus: Connection,
}
