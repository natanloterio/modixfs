use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::invocation::{EndpointSnapshot, InvocationSlot, SlotHandle, SlotKey};

/// Lookup table for live invocation slots.
///
/// Each `(ino, sid)` pair maps to one slot. The slot itself is wrapped
/// in an `Arc<Mutex<...>>` so callers can release the outer table lock
/// before doing per-slot work — important because handler invocation
/// can take seconds.
pub struct SlotTable {
    slots: Mutex<HashMap<SlotKey, SlotHandle>>,
}

impl SlotTable {
    pub fn new() -> Self {
        Self {
            slots: Mutex::new(HashMap::new()),
        }
    }

    /// Returns the existing slot for `key`, or creates one using
    /// `snapshot_fn()` if none exists.
    pub fn get_or_create<F>(&self, key: SlotKey, snapshot_fn: F) -> SlotHandle
    where
        F: FnOnce() -> EndpointSnapshot,
    {
        let mut g = self.slots.lock().unwrap();
        if let Some(h) = g.get(&key) {
            return h.clone();
        }
        let slot = Arc::new(Mutex::new(InvocationSlot::new(key, snapshot_fn())));
        g.insert(key, slot.clone());
        slot
    }

    /// Returns the slot for `key` if it exists, without creating one.
    pub fn get(&self, key: SlotKey) -> Option<SlotHandle> {
        self.slots.lock().unwrap().get(&key).cloned()
    }

    /// Removes and returns the slot for `key`, if any.
    pub fn remove(&self, key: SlotKey) -> Option<SlotHandle> {
        self.slots.lock().unwrap().remove(&key)
    }

    /// Removes every slot whose inode matches `ino`, regardless of sid.
    /// Used on `unlink` of a virtual endpoint.
    pub fn remove_all_for_ino(&self, ino: u64) -> usize {
        let mut g = self.slots.lock().unwrap();
        let to_remove: Vec<SlotKey> = g.keys().copied().filter(|(i, _)| *i == ino).collect();
        let n = to_remove.len();
        for k in to_remove {
            g.remove(&k);
        }
        n
    }

    /// Drops every slot whose `last_touched` is older than `max_idle`.
    /// Returns the number of slots reaped.
    pub fn reap_idle(&self, max_idle: Duration) -> usize {
        let now = std::time::Instant::now();
        let mut g = self.slots.lock().unwrap();
        let to_remove: Vec<SlotKey> = g
            .iter()
            .filter_map(|(k, h)| {
                let slot = h.lock().ok()?;
                if now.duration_since(slot.last_touched) > max_idle {
                    Some(*k)
                } else {
                    None
                }
            })
            .collect();
        let n = to_remove.len();
        for k in to_remove {
            g.remove(&k);
        }
        n
    }

    pub fn len(&self) -> usize {
        self.slots.lock().unwrap().len()
    }

    /// Returns the most recently completed result for `ino` across all
    /// sids, used by `.log` companion files whose owning slot may have
    /// been reaped. The returned bytes are the slot's `trace`.
    pub fn latest_trace_for_ino(&self, ino: u64) -> Option<Vec<u8>> {
        let g = self.slots.lock().unwrap();
        g.iter()
            .filter(|(k, _)| k.0 == ino)
            .filter_map(|(_, h)| {
                let s = h.lock().ok()?;
                if s.ready && !s.trace.is_empty() {
                    Some((s.last_touched, s.trace.clone()))
                } else {
                    None
                }
            })
            .max_by_key(|(t, _)| *t)
            .map(|(_, b)| b)
    }
}

impl Default for SlotTable {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::invocation::EndpointSnapshot;
    use crate::manifest::FileKind;
    use std::path::PathBuf;

    fn snap() -> EndpointSnapshot {
        EndpointSnapshot {
            tool_name: "t".into(),
            file_name: "ep".into(),
            cwd: PathBuf::from("/tmp"),
            kind: FileKind::WriteInvoke,
            handler: Some("cat".into()),
            input_schema: None,
            state_file: None,
            pipe: None,
            timeout_secs: 10,
            manifest_version: 0,
        }
    }

    #[test]
    fn get_or_create_returns_same_slot_for_same_key() {
        let t = SlotTable::new();
        let a = t.get_or_create((1, 100), snap);
        let b = t.get_or_create((1, 100), snap);
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn get_or_create_distinguishes_by_sid() {
        let t = SlotTable::new();
        let a = t.get_or_create((1, 100), snap);
        let b = t.get_or_create((1, 200), snap);
        assert!(!Arc::ptr_eq(&a, &b));
        assert_eq!(t.len(), 2);
    }

    #[test]
    fn get_or_create_distinguishes_by_ino() {
        let t = SlotTable::new();
        let a = t.get_or_create((1, 100), snap);
        let b = t.get_or_create((2, 100), snap);
        assert!(!Arc::ptr_eq(&a, &b));
        assert_eq!(t.len(), 2);
    }

    #[test]
    fn remove_drops_slot() {
        let t = SlotTable::new();
        t.get_or_create((1, 100), snap);
        assert!(t.remove((1, 100)).is_some());
        assert_eq!(t.len(), 0);
        assert!(t.remove((1, 100)).is_none());
    }

    #[test]
    fn remove_all_for_ino_drops_every_sid() {
        let t = SlotTable::new();
        t.get_or_create((1, 100), snap);
        t.get_or_create((1, 200), snap);
        t.get_or_create((2, 100), snap);
        let n = t.remove_all_for_ino(1);
        assert_eq!(n, 2);
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn reap_idle_drops_stale_slots() {
        let t = SlotTable::new();
        t.get_or_create((1, 100), snap);
        std::thread::sleep(Duration::from_millis(20));
        let n = t.reap_idle(Duration::from_millis(10));
        assert_eq!(n, 1);
        assert_eq!(t.len(), 0);
    }

    #[test]
    fn reap_idle_keeps_fresh_slots() {
        let t = SlotTable::new();
        let h = t.get_or_create((1, 100), snap);
        h.lock().unwrap().touch();
        let n = t.reap_idle(Duration::from_secs(60));
        assert_eq!(n, 0);
        assert_eq!(t.len(), 1);
    }
}
