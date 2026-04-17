use std::future::Future;
use std::sync::Arc;
use tokio::sync::watch;
use crate::detectors::context::DetectorContext;
use crate::event::Event;

pub trait Detector: Send + Sync + Clone {
    fn name(&self) -> &'static str;
    fn new(event_tx: tokio::sync::mpsc::Sender<Event>) -> Self
    where
        Self: Sized;
    fn start(self: Arc<Self>, ctx: DetectorContext, shutdown: watch::Receiver<bool>) -> impl Future<Output = anyhow::Result<()>> + Send;
    async fn stop(&self);
}
