//! What a child was started as — its `subagent/descriptor`, written once on the
//! child's own log.
//!
//! Ported from DSH `packages/subagent/subagent/src/descriptor.ts` without the
//! remote face. The descriptor is the child's answer to "how was I configured",
//! and it is what a cold resume reads to decide whether the child can be resumed
//! at all: a `one-shot` child cannot, a `continuable` child can. It is log-only:
//! it never enters model history and survives compaction, and it deliberately
//! does **not** carry depth or the output schema — depth is the envelope's job
//! and a schema belongs to the run that required it, not to the identity.

use hearth_core::event::{Event, EventType};
use serde_json::{json, Value};

/// The descriptor schema version this build writes. A record at another version
/// is not read (returned as `None`), matching DSH, which returns `undefined`.
pub const SUBAGENT_DESCRIPTOR_VERSION: u32 = 3;

/// How a child was created. `one-shot` runs once and is gone; `continuable` can
/// be resumed by id after it settles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    OneShot,
    Continuable,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::OneShot => "one-shot",
            Mode::Continuable => "continuable",
        }
    }

    pub fn parse(text: &str) -> Option<Mode> {
        match text {
            "one-shot" => Some(Mode::OneShot),
            "continuable" => Some(Mode::Continuable),
            _ => None,
        }
    }
}

/// A tool allow/deny filter, the way a persona narrows a child's tools.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ToolFilter {
    pub allow: Option<Vec<String>>,
    pub deny: Option<Vec<String>>,
}

/// The child's configuration record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Descriptor {
    pub mode: Mode,
    pub provider: String,
    pub label: Option<String>,
    pub agent_provider: Option<String>,
    pub agent_model: Option<String>,
    pub agent_reasoning_effort: Option<String>,
    pub persona: Option<String>,
    pub tool_filter: Option<ToolFilter>,
}

impl Descriptor {
    /// A local one-shot child: what this build's `Runner` creates today.
    pub fn one_shot(provider: impl Into<String>) -> Descriptor {
        Descriptor {
            mode: Mode::OneShot,
            provider: provider.into(),
            label: None,
            agent_provider: None,
            agent_model: None,
            agent_reasoning_effort: None,
            persona: None,
            tool_filter: None,
        }
    }

    /// A resumable local child with a label (the shape a continuation uses).
    pub fn continuable(provider: impl Into<String>, label: impl Into<String>) -> Descriptor {
        Descriptor {
            mode: Mode::Continuable,
            provider: provider.into(),
            label: Some(label.into()),
            agent_provider: None,
            agent_model: None,
            agent_reasoning_effort: None,
            persona: None,
            tool_filter: None,
        }
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Descriptor {
        self.agent_model = Some(model.into());
        self
    }

    /// The payload recorded on the log. Absent members are omitted, not null.
    pub fn to_payload(&self) -> Value {
        let mut payload = serde_json::Map::new();
        payload.insert("version".into(), json!(SUBAGENT_DESCRIPTOR_VERSION));
        payload.insert("mode".into(), json!(self.mode.as_str()));
        payload.insert("provider".into(), json!(self.provider));
        if let Some(label) = &self.label {
            payload.insert("label".into(), json!(label));
        }
        if let Some(value) = &self.agent_provider {
            payload.insert("agentProvider".into(), json!(value));
        }
        if let Some(value) = &self.agent_model {
            payload.insert("agentModel".into(), json!(value));
        }
        if let Some(value) = &self.agent_reasoning_effort {
            payload.insert("agentReasoningEffort".into(), json!(value));
        }
        if let Some(value) = &self.persona {
            payload.insert("persona".into(), json!(value));
        }
        if let Some(filter) = &self.tool_filter {
            let mut object = serde_json::Map::new();
            if let Some(allow) = &filter.allow {
                object.insert("allow".into(), json!(allow));
            }
            if let Some(deny) = &filter.deny {
                object.insert("deny".into(), json!(deny));
            }
            payload.insert("toolFilter".into(), Value::Object(object));
        }
        Value::Object(payload)
    }

    /// The record to append on the child's log.
    pub fn event(&self, session: impl Into<String>) -> Event {
        Event::new(
            session,
            EventType::SUBAGENT_DESCRIPTOR,
            Some(self.to_payload()),
        )
    }

    fn from_payload(value: &Value) -> Option<Descriptor> {
        let object = value.as_object()?;
        if object.get("version").and_then(Value::as_u64)? != SUBAGENT_DESCRIPTOR_VERSION as u64 {
            return None;
        }
        let mode = Mode::parse(object.get("mode").and_then(Value::as_str)?)?;
        let provider = object.get("provider").and_then(Value::as_str)?.to_string();
        let string = |key: &str| {
            object
                .get(key)
                .and_then(Value::as_str)
                .map(|s| s.to_string())
        };
        let tool_filter = object
            .get("toolFilter")
            .and_then(Value::as_object)
            .map(|filter| {
                let list = |key: &str| {
                    filter.get(key).and_then(Value::as_array).map(|items| {
                        items
                            .iter()
                            .filter_map(Value::as_str)
                            .map(|s| s.to_string())
                            .collect::<Vec<_>>()
                    })
                };
                ToolFilter {
                    allow: list("allow"),
                    deny: list("deny"),
                }
            });
        Some(Descriptor {
            mode,
            provider,
            label: string("label"),
            agent_provider: string("agentProvider"),
            agent_model: string("agentModel"),
            agent_reasoning_effort: string("agentReasoningEffort"),
            persona: string("persona"),
            tool_filter,
        })
    }
}

/// Fold a log's descriptor. The **first** `subagent/descriptor` record wins (it
/// is written once, at creation; later rewrites would be a second writer). A
/// record at an unknown version, or one whose required members are missing, folds
/// to `None` rather than guessing.
pub fn fold(events: &[Event]) -> Option<Descriptor> {
    for event in events {
        if event.typ != EventType::SUBAGENT_DESCRIPTOR.as_str() {
            continue;
        }
        return event.payload.as_ref().and_then(Descriptor::from_payload);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_one_shot_descriptor_round_trips_through_its_payload() {
        let descriptor = Descriptor::one_shot("local");
        let payload = descriptor.to_payload();
        assert_eq!(payload["version"], json!(3));
        assert_eq!(payload["mode"], json!("one-shot"));
        assert_eq!(payload["provider"], json!("local"));
        assert!(payload.get("label").is_none(), "absent members are omitted");
        assert_eq!(Descriptor::from_payload(&payload), Some(descriptor));
    }

    #[test]
    fn a_continuable_descriptor_carries_its_label_and_optional_knobs() {
        let descriptor = Descriptor::continuable("local", "research").with_model("gpt-5");
        let payload = descriptor.to_payload();
        assert_eq!(payload["mode"], json!("continuable"));
        assert_eq!(payload["label"], json!("research"));
        assert_eq!(payload["agentModel"], json!("gpt-5"));
        assert_eq!(Descriptor::from_payload(&payload), Some(descriptor));
    }

    #[test]
    fn fold_reads_the_first_descriptor_and_ignores_other_types() {
        let first = Descriptor::continuable("local", "a").event("s_child");
        let second = Descriptor::continuable("local", "b").event("s_child");
        let mut events = vec![
            Event::new("s_child", EventType::SESSION_START, None),
            first,
            second,
        ];
        for (index, event) in events.iter_mut().enumerate() {
            event.seq = index as u64 + 1;
        }
        let found = fold(&events).expect("a descriptor");
        assert_eq!(found.label.as_deref(), Some("a"), "first wins");
    }

    #[test]
    fn an_unknown_version_or_a_missing_member_folds_to_none() {
        let mut payload = Descriptor::one_shot("local").to_payload();
        payload["version"] = json!(99);
        let event = Event::new("s_child", EventType::SUBAGENT_DESCRIPTOR, Some(payload));
        assert_eq!(fold(&[event]), None);

        let broken = Event::new(
            "s_child",
            EventType::SUBAGENT_DESCRIPTOR,
            Some(json!({"version": 3, "mode": "one-shot"})),
        );
        assert_eq!(fold(&[broken]), None, "provider is required");
    }
}
