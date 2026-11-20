//! The `structured_output` terminal tool and a deliberately tiny JSON-schema
//! subset validator.
//!
//! A child normally reports its result as free text. When the caller gives it a
//! schema, the child finishes by CALLING `structured_output` with arguments that
//! satisfy that schema; the parent then reads the arguments back out of the
//! child's own log (`tool/call.args`), exactly like it already reads the final
//! text. Calling the tool completes the task: every later tool call is rejected
//! by [`GuardedTool`], and the loop refuses a second structured submission.
//!
//! Why there is no JSON-schema crate: the workspace dependency budget is nine
//! crates and none of them is a schema engine. We do not need all of JSON
//! Schema — we need to decide, at the moment the child hands its result back,
//! whether it is right. A two-hundred-line subset over `type`/`required`/
//! `properties`/`items` is smaller and easier to audit than dragging in a schema
//! engine plus a validator dependency, and it fails closed on the shapes this
//! crate actually advertises. It is NOT a general JSON Schema implementation;
//! `README.md` says so under “不要做”.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use hearth_llm::ToolSpec;
use hearth_tool::{Call, Result as ToolResult, Tool, ERR_BAD_ARGS};
use serde_json::{json, Value};

/// Wire name of the terminal tool.
pub const STRUCTURED_OUTPUT_TOOL: &str = "structured_output";

/// Code returned when a tool is called after `structured_output` has completed
/// the child. Distinct from an ordinary argument error so a parent can tell
/// “the task was already finished” from “you typed the arguments wrong”.
pub const CODE_TERMINATED: &str = "already_terminated";

const DESCRIPTION: &str = "Submit the final result as JSON that matches the required schema. Calling this tool completes the task; any tool call after it is rejected.";

/// The advertised tool spec for one child: the tool's input schema IS the
/// caller-supplied result schema, so the model's arguments are the result.
pub fn spec(schema: &Value) -> ToolSpec {
    ToolSpec {
        name: STRUCTURED_OUTPUT_TOOL.to_string(),
        description: DESCRIPTION.to_string(),
        schema: schema.clone(),
    }
}

/// The `structured_output` terminal tool. It owns the “terminated” flag shared
/// with every other tool in the child registry.
pub(crate) struct StructuredOutputTool {
    pub(crate) schema: Value,
    pub(crate) terminated: Arc<AtomicBool>,
}

impl Tool for StructuredOutputTool {
    fn spec(&self) -> ToolSpec {
        spec(&self.schema)
    }

    fn call(&self, call: Call) -> ToolResult {
        if self.terminated.load(Ordering::SeqCst) {
            return ToolResult::error(
                CODE_TERMINATED,
                format!("{STRUCTURED_OUTPUT_TOOL}: the task is already complete; no further tool calls are accepted"),
            );
        }
        let value = if call.args.is_null() {
            json!({})
        } else {
            call.args.clone()
        };
        match validate(&self.schema, &value) {
            Ok(()) => {
                self.terminated.store(true, Ordering::SeqCst);
                ToolResult::ok(format!(
                    "structured result accepted ({})",
                    json_type(&value)
                ))
                .with_meta("structured", value)
            }
            Err(message) => {
                ToolResult::error(ERR_BAD_ARGS, format!("{STRUCTURED_OUTPUT_TOOL}: {message}"))
            }
        }
    }
}

/// Wraps every non-terminal child tool so that, once a structured result has
/// been accepted, later calls fail closed instead of running.
pub(crate) struct GuardedTool {
    pub(crate) inner: Arc<dyn Tool>,
    pub(crate) terminated: Arc<AtomicBool>,
}

impl Tool for GuardedTool {
    fn spec(&self) -> ToolSpec {
        self.inner.spec()
    }

    fn call(&self, call: Call) -> ToolResult {
        if self.terminated.load(Ordering::SeqCst) {
            return ToolResult::error(
                CODE_TERMINATED,
                format!(
                    "{}: the structured result was already submitted; this call is rejected",
                    call.name
                ),
            );
        }
        self.inner.call(call)
    }
}

/// Validate `value` against the supported JSON-schema subset.
///
/// Supported keywords, all optional: `type`, `required`, `properties`, `items`.
/// Supported `type` names: `object`, `array`, `string`, `number`, `integer`,
/// `boolean` (and `null`). A schema that is not an object, or a `type` name
/// outside that list, is accepted — the subset deliberately does not model every
/// JSON Schema construct. Everything supported fails closed: a missing required
/// property, a mismatched type, or a bad array element is an error naming the
/// path (`$.name`, `$.tags[1]`).
pub(crate) fn validate(schema: &Value, value: &Value) -> std::result::Result<(), String> {
    validate_at(schema, value, "$")
}

fn validate_at(schema: &Value, value: &Value, path: &str) -> std::result::Result<(), String> {
    let object = match schema.as_object() {
        Some(object) => object,
        None => return Ok(()),
    };

    if let Some(expected) = object.get("type") {
        if !type_matches(expected, value) {
            return Err(format!(
                "{path}: expected {}, got {}",
                type_names(expected),
                json_type(value)
            ));
        }
    }

    if let Some(map) = value.as_object() {
        if let Some(required) = object.get("required").and_then(Value::as_array) {
            for key in required {
                if let Some(key) = key.as_str() {
                    if !map.contains_key(key) {
                        return Err(format!("{path}: missing required property `{key}`"));
                    }
                }
            }
        }
        if let Some(properties) = object.get("properties").and_then(Value::as_object) {
            for (key, subschema) in properties {
                if let Some(child) = map.get(key) {
                    validate_at(subschema, child, &format!("{path}.{key}"))?;
                }
            }
        }
    }

    if let Some(items) = object.get("items") {
        if let Some(array) = value.as_array() {
            for (index, item) in array.iter().enumerate() {
                validate_at(items, item, &format!("{path}[{index}]"))?;
            }
        }
    }

    Ok(())
}

fn type_matches(expected: &Value, value: &Value) -> bool {
    match expected {
        Value::String(name) => matches_type(name, value),
        Value::Array(names) => names
            .iter()
            .filter_map(Value::as_str)
            .any(|name| matches_type(name, value)),
        _ => true,
    }
}

fn matches_type(name: &str, value: &Value) -> bool {
    match name {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => is_integer(value),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        // An unknown type name is outside the subset: do not reject on it.
        _ => true,
    }
}

fn is_integer(value: &Value) -> bool {
    match value {
        Value::Number(number) => {
            number.is_i64()
                || number.is_u64()
                || number
                    .as_f64()
                    .map(|f| f.is_finite() && f.fract() == 0.0)
                    .unwrap_or(false)
        }
        _ => false,
    }
}

fn type_names(expected: &Value) -> String {
    match expected {
        Value::String(name) => name.clone(),
        Value::Array(names) => names
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join("|"),
        _ => "any".to_string(),
    }
}

fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(number) if number.is_i64() || number.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "age": { "type": "integer" },
                "tags": { "type": "array", "items": { "type": "string" } }
            },
            "required": ["name", "tags"]
        })
    }

    #[test]
    fn accepts_a_matching_object() {
        let value = json!({"name": "ada", "age": 36, "tags": ["math", "code"]});
        assert!(validate(&schema(), &value).is_ok());
    }

    #[test]
    fn accepts_object_without_optional_properties() {
        let value = json!({"name": "ada", "tags": []});
        assert!(validate(&schema(), &value).is_ok());
    }

    #[test]
    fn rejects_a_missing_required_property() {
        let err = validate(&schema(), &json!({"name": "ada"})).unwrap_err();
        assert!(err.contains("missing required property `tags`"), "{err}");
    }

    #[test]
    fn rejects_a_wrong_top_level_type() {
        let err = validate(&schema(), &json!("not an object")).unwrap_err();
        assert!(err.contains("expected object"), "{err}");
    }

    #[test]
    fn rejects_a_wrong_property_type() {
        let err = validate(&schema(), &json!({"name": 7, "tags": []})).unwrap_err();
        assert!(err.contains("$.name: expected string"), "{err}");
    }

    #[test]
    fn rejects_a_bad_item_in_an_array_recursively() {
        let err = validate(&schema(), &json!({"name": "ada", "tags": ["ok", 3]})).unwrap_err();
        assert!(err.contains("$.tags[1]: expected string"), "{err}");
    }

    #[test]
    fn integer_accepts_whole_numbers_and_rejects_fractions() {
        let integer = json!({"type": "integer"});
        assert!(validate(&integer, &json!(3)).is_ok());
        assert!(validate(&integer, &json!(3.0)).is_ok());
        assert!(validate(&integer, &json!(3.5)).is_err());
        assert!(validate(&integer, &json!("3")).is_err());
    }

    #[test]
    fn number_accepts_integers_and_fractions() {
        let number = json!({"type": "number"});
        assert!(validate(&number, &json!(3)).is_ok());
        assert!(validate(&number, &json!(3.5)).is_ok());
        assert!(validate(&number, &json!(true)).is_err());
    }

    #[test]
    fn boolean_and_null_are_supported() {
        assert!(validate(&json!({"type": "boolean"}), &json!(true)).is_ok());
        assert!(validate(&json!({"type": "boolean"}), &json!(0)).is_err());
        assert!(validate(&json!({"type": "null"}), &Value::Null).is_ok());
    }

    #[test]
    fn a_non_object_schema_is_accepted() {
        assert!(validate(&json!(true), &json!(42)).is_ok());
    }

    #[test]
    fn a_type_union_accepts_either() {
        let union = json!({"type": ["string", "integer"]});
        assert!(validate(&union, &json!("x")).is_ok());
        assert!(validate(&union, &json!(2)).is_ok());
        assert!(validate(&union, &json!(true)).is_err());
    }

    // --- terminal tool + guard -------------------------------------------

    use hearth_llm::ToolSpec;
    use hearth_tool::{Call, Result as ToolResult, Tool};

    struct EchoTool;

    impl Tool for EchoTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "echo".to_string(),
                description: "echo".to_string(),
                schema: json!({"type": "object"}),
            }
        }

        fn call(&self, _call: Call) -> ToolResult {
            ToolResult::ok("echoed")
        }
    }

    fn terminating_tool() -> (StructuredOutputTool, Arc<AtomicBool>) {
        let flag = Arc::new(AtomicBool::new(false));
        (
            StructuredOutputTool {
                schema: json!({"type": "object", "required": ["x"]}),
                terminated: Arc::clone(&flag),
            },
            flag,
        )
    }

    #[test]
    fn a_valid_submission_terminates_the_child_and_guards_later_calls() {
        let (tool, flag) = terminating_tool();
        let submitted = json!({"x": 1});
        let accepted = tool.call(Call::new(
            "c1",
            STRUCTURED_OUTPUT_TOOL,
            submitted.clone(),
            "",
        ));
        assert!(accepted.is_ok(), "{accepted:?}");
        assert_eq!(accepted.meta["structured"], submitted);
        assert!(flag.load(Ordering::SeqCst));

        // A later ordinary tool call is rejected by the shared guard.
        let guarded = GuardedTool {
            inner: Arc::new(EchoTool),
            terminated: Arc::clone(&flag),
        };
        let denied = guarded.call(Call::new("c2", "echo", json!({}), ""));
        assert!(!denied.is_ok());
        assert_eq!(denied.code, CODE_TERMINATED);

        // A second structured submission is refused too, not silently accepted.
        let again = tool.call(Call::new("c3", STRUCTURED_OUTPUT_TOOL, json!({"x": 2}), ""));
        assert_eq!(again.code, CODE_TERMINATED);
    }

    #[test]
    fn invalid_arguments_are_rejected_and_do_not_terminate() {
        let (tool, flag) = terminating_tool();
        let bad = tool.call(Call::new("c1", STRUCTURED_OUTPUT_TOOL, json!({}), ""));
        assert!(!bad.is_ok());
        assert_eq!(bad.code, ERR_BAD_ARGS);
        assert!(
            !flag.load(Ordering::SeqCst),
            "a rejected submission must not terminate"
        );

        // After a rejection the child can still submit a valid object.
        let good = tool.call(Call::new("c2", STRUCTURED_OUTPUT_TOOL, json!({"x": 1}), ""));
        assert!(good.is_ok(), "{good:?}");
        assert!(flag.load(Ordering::SeqCst));
    }
}
