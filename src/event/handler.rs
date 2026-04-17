use crate::event::Event;

pub trait EventHandler: Send + Sync {
    fn handle(&self, event: &Event) -> anyhow::Result<()>;
}

impl<T: EventHandler> EventHandler for std::sync::Arc<T> {
    fn handle(&self, event: &Event) -> anyhow::Result<()> {
        (**self).handle(event)
    }
}

impl<T: EventHandler> EventHandler for Box<T> {
    fn handle(&self, event: &Event) -> anyhow::Result<()> {
        (**self).handle(event)
    }
}
