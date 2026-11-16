//! Name-to-tool registry, ported from `archive/forge-go/internal/tool/registry.go`.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use forge_llm::ToolSpec;

use crate::Tool;

/// A concurrent registry with no built-in names. Callers choose what exists.
#[derive(Clone, Default)]
pub struct Registry {
    tools: Arc<RwLock<BTreeMap<String, Arc<dyn Tool>>>>,
}

impl Registry {
    pub fn new(tools: impl IntoIterator<Item = Arc<dyn Tool>>) -> Self {
        let registry = Self::default();
        for tool in tools {
            registry.register_arc(tool);
        }
        registry
    }

    /// Register a concrete tool. Rust has no nil trait object, so the nil case
    /// from Go is structurally impossible.
    pub fn register<T: Tool + 'static>(&self, tool: T) {
        self.register_arc(Arc::new(tool));
    }

    pub fn register_arc(&self, tool: Arc<dyn Tool>) {
        let name = tool.spec().name;
        if name.is_empty() {
            panic!("tool: Register: empty Spec().Name()");
        }
        let mut tools = self.tools.write().unwrap_or_else(|err| err.into_inner());
        if tools.contains_key(&name) {
            panic!("duplicate tool name {name:?}");
        }
        tools.insert(name, tool);
    }

    pub fn lookup(&self, name: &str) -> Option<Arc<dyn Tool>> {
        let tools = self.tools.read().unwrap_or_else(|err| err.into_inner());
        tools.get(name).cloned()
    }

    /// Sorted by name because the model sees a stable list.
    pub fn specs(&self) -> Vec<ToolSpec> {
        let tools = self.tools.read().unwrap_or_else(|err| err.into_inner());
        tools.values().map(|tool| tool.spec()).collect()
    }

    pub fn names(&self) -> Vec<String> {
        let tools = self.tools.read().unwrap_or_else(|err| err.into_inner());
        tools.keys().cloned().collect()
    }

    pub fn len(&self) -> usize {
        let tools = self.tools.read().unwrap_or_else(|err| err.into_inner());
        tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Call, Result};
    use serde_json::json;

    struct Named(&'static str);

    impl Tool for Named {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: self.0.to_string(),
                description: self.0.to_string(),
                schema: json!({"type":"object"}),
            }
        }

        fn call(&self, _call: Call) -> Result {
            Result::ok(self.0)
        }
    }

    #[test]
    fn names_are_sorted_and_lists_are_copies() {
        let registry = Registry::new(vec![
            Arc::new(Named("z")) as Arc<dyn Tool>,
            Arc::new(Named("a")) as Arc<dyn Tool>,
        ]);
        assert_eq!(registry.names(), vec!["a", "z"]);
        let mut names = registry.names();
        names.clear();
        assert_eq!(registry.names(), vec!["a", "z"]);
    }

    #[test]
    #[should_panic(expected = "duplicate tool name \"a\"")]
    fn duplicate_name_panics() {
        let registry = Registry::new(vec![Arc::new(Named("a")) as Arc<dyn Tool>]);
        registry.register(Named("a"));
    }

    #[test]
    #[should_panic(expected = "empty Spec().Name()")]
    fn empty_name_panics() {
        let registry = Registry::default();
        registry.register(Named(""));
    }
}
