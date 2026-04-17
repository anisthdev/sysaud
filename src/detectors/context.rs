use crate::config::SharedConfig;
use crate::event::Event;
use zbus::Connection;

#[derive(Clone)]
pub struct DetectorContext {
    pub event_tx: tokio::sync::mpsc::Sender<Event>,
    pub config: SharedConfig,
    pub session_bus: Connection,
    pub system_bus: Connection,
}
