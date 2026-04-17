use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateTransition<T> {
    Initialized,
    Unchanged,
    Changed { previous: T },
}

#[derive(Clone, Default)]
pub struct StateCell<T> {
    inner: Arc<Mutex<Option<T>>>,
}

impl<T> StateCell<T> {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(None)),
        }
    }
}

impl<T> StateCell<T>
where
    T: Clone + PartialEq,
{
    pub async fn update(&self, next: T) -> StateTransition<T> {
        let mut guard = self.inner.lock().await;

        match guard.as_ref() {
            None => {
                *guard = Some(next);
                StateTransition::Initialized
            }
            Some(current) if current == &next => StateTransition::Unchanged,
            Some(current) => {
                let previous = current.clone();
                *guard = Some(next);
                StateTransition::Changed { previous }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn update_reports_transitions() {
        let cell = StateCell::new();

        assert!(matches!(cell.update("a".to_string()).await, StateTransition::Initialized));
        assert!(matches!(cell.update("a".to_string()).await, StateTransition::Unchanged));

        match cell.update("b".to_string()).await {
            StateTransition::Changed { previous } => assert_eq!(previous, "a"),
            other => panic!("unexpected transition: {other:?}"),
        }
    }
}
