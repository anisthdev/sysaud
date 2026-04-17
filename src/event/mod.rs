use std::collections::HashMap;

pub mod handler;
pub mod router;

pub use handler::EventHandler;

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct EventId {
    pub category: String,
    pub name: String,
}

impl EventId {
    pub fn new(category: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            category: category.into(),
            name: name.into(),
        }
    }

    pub fn as_str(&self) -> String {
        format!("{}.{}", self.category, self.name)
    }
}

impl std::fmt::Display for EventId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.category, self.name)
    }
}

#[derive(Debug, Clone)]
pub struct Event {
    pub id: EventId,
    pub detail: HashMap<String, String>,
}

impl Event {
    pub fn new(
        category: impl Into<String>,
        name: impl Into<String>,
        detail: impl Into<HashMap<String, String>>,
    ) -> Self {
        let id = EventId::new(category, name);
        Self {
            detail: detail.into(),
            id,
        }
    }
}
