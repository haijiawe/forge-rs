//! One lock per child, so two callers cannot rewrite one child's log at the
//! same time.
//!
//! A child is a session log. Appending two turns to it concurrently would
//! interleave `turn/start`/`turn/end` pairs and break the "dense, ordered" rules
//! the projection relies on. DSH serialises deliveries with a per-child lock
//! (`ChildLock.run`); this is the same idea, keyed by the child id.
//!
//! The lock is created lazily per id and never removed: the map grows with the
//! number of distinct children this process talked to, which is bounded by the
//! work the process did. Removing entries would need a reference count to be
//! correct, and a wrong removal reintroduces the very race it prevents.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Serialises operations that append to one child's log.
#[derive(Clone, Default)]
pub struct ChildLock {
    inner: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
}

impl ChildLock {
    pub fn new() -> Self {
        Self::default()
    }

    fn entry(&self, child_id: &str) -> Arc<Mutex<()>> {
        let mut map = match self.inner.lock() {
            Ok(guard) => guard,
            // A poisoned map is not corrupt data — the values are plain mutexes —
            // so recovering it is the honest move, not panicking on it.
            Err(poisoned) => poisoned.into_inner(),
        };
        Arc::clone(
            map.entry(child_id.to_string())
                .or_insert_with(|| Arc::new(Mutex::new(()))),
        )
    }

    /// Run `op` while holding the lock for `child_id`.
    pub fn run<T>(&self, child_id: &str, op: impl FnOnce() -> T) -> T {
        let slot = self.entry(child_id);
        let _guard = match slot.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        op()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn two_threads_never_hold_the_same_child_at_once() {
        let lock = ChildLock::new();
        let inside = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let lock = lock.clone();
            let inside = Arc::clone(&inside);
            let peak = Arc::clone(&peak);
            handles.push(thread::spawn(move || {
                lock.run("child-1", || {
                    let now = inside.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    thread::sleep(std::time::Duration::from_millis(5));
                    inside.fetch_sub(1, Ordering::SeqCst);
                });
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(
            peak.load(Ordering::SeqCst),
            1,
            "the lock let two runs overlap"
        );
    }

    #[test]
    fn different_children_do_not_block_each_other() {
        let lock = ChildLock::new();
        let order = Arc::new(Mutex::new(Vec::<&'static str>::new()));
        let a = {
            let lock = lock.clone();
            let order = Arc::clone(&order);
            thread::spawn(move || lock.run("a", || order.lock().unwrap().push("a")))
        };
        let b = {
            let lock = lock.clone();
            let order = Arc::clone(&order);
            thread::spawn(move || lock.run("b", || order.lock().unwrap().push("b")))
        };
        a.join().unwrap();
        b.join().unwrap();
        assert_eq!(order.lock().unwrap().len(), 2);
    }
}
