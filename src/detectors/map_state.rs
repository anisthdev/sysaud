use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Arc;
use parking_lot::Mutex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MapTransition<V> {
    Inserted,
    Unchanged,
    Updated { previous: V },
}

#[derive(Clone, Default)]
pub struct StateMap<K, V> {
    inner: Arc<Mutex<HashMap<K, V>>>,
}

impl<K, V> StateMap<K, V>
where
    K: Eq + Hash,
{
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl<K, V> StateMap<K, V>
where
    K: Eq + Hash + Clone,
    V: Clone + PartialEq,
{
    pub fn update(&self, key: K, next: V) -> MapTransition<V> {
        let mut guard = self.inner.lock();
        match guard.get(&key) {
            None => {
                guard.insert(key, next);
                MapTransition::Inserted
            }
            Some(current) if current == &next => MapTransition::Unchanged,
            Some(current) => {
                let previous = current.clone();
                guard.insert(key, next);
                MapTransition::Updated { previous }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_reports_keyed_transitions() {
        let map = StateMap::new();

        assert!(matches!(map.update("a", 1), MapTransition::Inserted));
        assert!(matches!(map.update("a", 1), MapTransition::Unchanged));

        match map.update("a", 2) {
            MapTransition::Updated { previous } => assert_eq!(previous, 1),
            other => panic!("unexpected transition: {other:?}"),
        }
    }
}
