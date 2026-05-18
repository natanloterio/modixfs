use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Per-mount session state shared across all tool invocations.
#[derive(Clone, Default)]
pub struct Session {
    state: Arc<Mutex<HashMap<String, Vec<u8>>>>,
}

impl Session {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, key: &str) -> Option<Vec<u8>> {
        self.state.lock().unwrap().get(key).cloned()
    }

    pub fn set(&self, key: impl Into<String>, value: impl Into<Vec<u8>>) {
        self.state.lock().unwrap().insert(key.into(), value.into());
    }

    pub fn remove(&self, key: &str) {
        self.state.lock().unwrap().remove(key);
    }
}
