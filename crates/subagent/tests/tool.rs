//! Tool-surface tests mirrored from `forge-v2/internal/subagent/tool_test.go`.

mod common;

use forge_llm::{Message, Request};
use forge_subagent::{spec, tool_schema, CODE_DEPTH_EXCEEDED, TOOL_NAME};
use serde_json::json;

#[test]
fn spec_is_valid_and_acceptable_to_the_provider_request() {
    let s = spec();
    assert_eq!(s.name, TOOL_NAME);
    assert!(s.description.contains("Delegate"));
    assert!(s.description.contains("does not see this conversation"));
    assert!(s.description.contains("approval"));
    assert!(!s.description.contains('\n'));

    let schema = s.schema.as_object().expect("object schema");
    assert_eq!(schema.get("type"), Some(&json!("object")));
    assert_eq!(schema.get("additionalProperties"), Some(&json!(false)));
    assert_eq!(schema.get("required"), Some(&json!(["prompt"])));
    let props = schema
        .get("properties")
        .and_then(|p| p.as_object())
        .expect("properties");
    assert!(props.contains_key("prompt"));
    assert!(props.contains_key("system"));
    assert!(props.contains_key("model"));

    let req = Request {
        model: "test-model".into(),
        messages: vec![Message::user("hello")],
        tools: vec![s],
        ..Request::default()
    };
    req.validate()
        .expect("request with the subagent tool validates");
}

#[test]
fn tool_schema_is_a_fresh_tree_on_every_call() {
    let mut a = tool_schema();
    let b = tool_schema();
    if let Some(obj) = a.as_object_mut() {
        obj.insert("poisoned".into(), json!(true));
    }
    assert!(b.get("poisoned").is_none(), "a cached schema was mutated");
    assert_eq!(b, tool_schema());
}

#[test]
fn description_says_when_not_to_delegate() {
    let description = spec().description.to_lowercase();
    assert!(description.contains("small"));
    assert!(description.contains("do not delegate"));
    assert!(description.contains("approval"));
    assert!(description.contains("context"));
}

#[test]
fn depth_exceeded_code_is_exported_and_stable() {
    assert_eq!(CODE_DEPTH_EXCEEDED, "depth_exceeded");
    assert_eq!(TOOL_NAME, "subagent");
}
