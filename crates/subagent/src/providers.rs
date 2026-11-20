//! A registry that turns a backend name into a backend.
//!
//! The subagent runner has one built-in way to run a child (in this process).
//! Any other execution shape — another process, a remote worker, a test double —
//! arrives as a [`Backend`], and this is where those are named and looked up.
//! DSH calls this the provider registry (`SubagentProvider`); the shape is the
//! same: a provider has a name, you register it once, and the runner asks for it
//! by name.
//!
//! The registry holds only external providers. The in-process path is not a
//! `Backend` — it writes a local log and shares the tools — so "no provider
//! named X" correctly means "fall back to in-process", not "fail".

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use forge_core::{Error, ErrorKind, Result};

use crate::backend::Backend;

/// Named external providers.
#[derive(Clone, Default)]
pub struct Providers {
    inner: Arc<Mutex<BTreeMap<String, Arc<dyn Backend>>>>,
}

impl Providers {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a backend under its own `name()`. A second provider with the
    /// same name is refused: silently replacing one would make "which backend
    /// ran my child" a function of registration order.
    pub fn register(&self, backend: Arc<dyn Backend>) -> Result<()> {
        // Own the name before moving `backend` in: `backend.name()` borrows it.
        let name = backend.name().to_owned();
        self.register_named(&name, backend)
    }

    /// Register a backend under an explicit name (for a provider whose `name()`
    /// is a fixed type name but that the operator wants to alias).
    pub fn register_named(&self, name: &str, backend: Arc<dyn Backend>) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            return Err(Error::new(
                ErrorKind::Invalid,
                "subagent: a provider needs a non-empty name",
            ));
        }
        let mut map = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if map.contains_key(name) {
            return Err(Error::new(
                ErrorKind::Conflict,
                format!("subagent: a provider named {name:?} is already registered"),
            ));
        }
        map.insert(name.to_string(), backend);
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Backend>> {
        let map = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        map.get(name).map(Arc::clone)
    }

    pub fn names(&self) -> Vec<String> {
        let map = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        map.keys().cloned().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn len(&self) -> usize {
        match self.inner.lock() {
            Ok(guard) => guard.len(),
            Err(poisoned) => poisoned.into_inner().len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{BackendOutcome, BackendRequest};

    struct Named(&'static str);

    impl Backend for Named {
        fn name(&self) -> &str {
            self.0
        }
        fn call(&self, _request: &BackendRequest) -> Result<BackendOutcome> {
            Ok(BackendOutcome::failure("test", "not run"))
        }
    }

    #[test]
    fn a_registered_provider_is_found_by_name() {
        let providers = Providers::new();
        providers.register(Arc::new(Named("worker"))).unwrap();
        assert_eq!(providers.names(), vec!["worker".to_string()]);
        assert!(providers.get("worker").is_some());
        assert!(providers.get("local").is_none());
    }

    #[test]
    fn a_duplicate_name_is_refused_rather_than_replaced() {
        let providers = Providers::new();
        providers.register(Arc::new(Named("worker"))).unwrap();
        let err = providers
            .register(Arc::new(Named("worker")))
            .expect_err("duplicate must be refused");
        assert_eq!(err.kind(), ErrorKind::Conflict);
        assert_eq!(providers.len(), 1);
    }

    #[test]
    fn an_empty_name_is_invalid() {
        let providers = Providers::new();
        let err = providers
            .register_named("   ", Arc::new(Named("x")))
            .expect_err("empty name must be refused");
        assert_eq!(err.kind(), ErrorKind::Invalid);
    }
}
