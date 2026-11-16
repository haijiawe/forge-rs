//! A stateless plan renderer. The session log, not this tool, owns the plan.

use serde::Deserialize;
use serde_json::json;

use crate::contract::{decode_args, Call, Result, ERR_BAD_ARGS};
use crate::truncate::{truncate, Limits};
use crate::Tool;

const PENDING: &str = "pending";
const IN_PROGRESS: &str = "in_progress";
const COMPLETED: &str = "completed";

pub struct TodoWriteTool {
    limits: Limits,
}

#[derive(Deserialize)]
struct TodoArgs {
    #[serde(default)]
    items: Vec<TodoItem>,
}

#[derive(Deserialize)]
struct TodoItem {
    text: String,
    status: String,
}

impl TodoWriteTool {
    pub fn new(limits: Limits) -> Self {
        Self { limits }
    }

    fn effective_limits(&self) -> Limits {
        if self.limits.max_bytes <= 0 && self.limits.max_lines <= 0 {
            Limits::default()
        } else {
            self.limits
        }
    }
}

impl Default for TodoWriteTool {
    fn default() -> Self {
        Self::new(Limits::default())
    }
}

impl Tool for TodoWriteTool {
    fn spec(&self) -> forge_llm::ToolSpec {
        forge_llm::ToolSpec {
            name: "todo_write".into(),
            description: "State the plan for a multi-step task and show the user where you are in it.\n\nWhen to use: the task needs three or more distinct steps, the user asked for a plan, or you are handing the same job back and forth between sub-tasks and need to say what is left. Skip it for a single obvious step — a one-item list is noise.\n\nHow to use it: send the whole list on every call; this call replaces the plan, it does not append to it. Keep exactly one item in_progress while you work on it, and mark an item completed as soon as it is done rather than in a batch at the end. An item that turned out to be unnecessary is still worth marking completed with the reason in its text.\n\nStorage: this tool persists nothing. The harness records the call and this result in the session log, which is the only source of truth, and the plan is carried forward in the conversation. Nothing here survives outside that.\n\nAn empty list is rejected, and so is an unknown status. Statuses are pending, in_progress and completed. The output repeats the counts so you can keep your place.\n\nExample arguments:\n{\"items\":[{\"text\":\"Read internal/tool/contract.go\",\"status\":\"completed\"},{\"text\":\"Implement the shell tool\",\"status\":\"in_progress\"},{\"text\":\"Run go test ./internal/tool\",\"status\":\"pending\"}]}".into(),
            schema: json!({
                "type":"object",
                "properties":{
                    "items":{
                        "type":"array",
                        "description":"The complete plan, in the order the steps should be read. Replaces any previous plan.",
                        "minItems":1,
                        "items":{
                            "type":"object",
                            "properties":{
                                "text":{"type":"string","description":"One line describing the step, in the user's language."},
                                "status":{"type":"string","enum":["pending","in_progress","completed"],"description":"pending: not started; in_progress: the step being worked on now (at most one); completed: done."}
                            },
                            "required":["text","status"],
                            "additionalProperties":false
                        }
                    }
                },
                "required":["items"],
                "additionalProperties":false
            }),
        }
    }

    fn call(&self, call: Call) -> Result {
        let args: TodoArgs = match decode_args(&call) {
            Ok(args) => args,
            Err(result) => return result,
        };
        if args.items.is_empty() {
            return Result::error(ERR_BAD_ARGS, "todo_write: items is empty; pass the complete plan as a list with at least one item");
        }

        let mut items = Vec::with_capacity(args.items.len());
        let mut pending = 0usize;
        let mut in_progress = 0usize;
        let mut completed = 0usize;
        for (index, raw) in args.items.into_iter().enumerate() {
            let text = raw.text.trim().to_string();
            if text.is_empty() {
                return Result::error(
                    ERR_BAD_ARGS,
                    format!("todo_write: item {} has empty text; every item needs a one-line description of the step", index + 1),
                );
            }
            let status = raw.status.trim().to_lowercase();
            match status.as_str() {
                PENDING => pending += 1,
                IN_PROGRESS => in_progress += 1,
                COMPLETED => completed += 1,
                _ => {
                    return Result::error(
                        ERR_BAD_ARGS,
                        format!(
                            "todo_write: item {} ({:?}) has unknown status {:?}; use one of {PENDING}, {IN_PROGRESS}, {COMPLETED}",
                            index + 1,
                            text,
                            raw.status
                        ),
                    )
                }
            }
            items.push((status, text));
        }

        let mut output = format!(
            "todo list: {pending} pending, {in_progress} in progress, {completed} completed ({} items)\n",
            items.len()
        );
        for (status, text) in &items {
            output.push_str(&format!("{} {text}\n", mark(status)));
        }
        let (output, truncated) = truncate(&output, self.effective_limits());
        let mut result = Result::ok(output);
        result.truncated = truncated;
        result.meta.insert(PENDING.to_string(), json!(pending));
        result
            .meta
            .insert(IN_PROGRESS.to_string(), json!(in_progress));
        result.meta.insert(COMPLETED.to_string(), json!(completed));
        result.meta.insert("total".to_string(), json!(items.len()));
        result
    }
}

fn mark(status: &str) -> &'static str {
    match status {
        COMPLETED => "[x]",
        IN_PROGRESS => "[~]",
        _ => "[ ]",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(args: serde_json::Value) -> Call {
        Call {
            id: "c1".into(),
            name: "todo_write".into(),
            args,
            workspace: ".".into(),
        }
    }

    #[test]
    fn normalizes_and_counts() {
        let result = TodoWriteTool::default().call(call(json!({
            "items":[
                {"text":" Read code ","status":" COMPLETED "},
                {"text":"Write Rust","status":"in_progress"},
                {"text":"Run tests","status":"pending"}
            ]
        })));
        assert!(result
            .output
            .starts_with("todo list: 1 pending, 1 in progress, 1 completed (3 items)"));
        assert!(result.output.contains("[x] Read code"));
        assert_eq!(result.meta.get("total"), Some(&json!(3)));
    }

    #[test]
    fn rejects_empty_and_unknown_status() {
        let empty = TodoWriteTool::default().call(call(json!({"items":[]})));
        assert_eq!(empty.code, ERR_BAD_ARGS);
        let bad = TodoWriteTool::default().call(call(json!({
            "items":[{"text":"x","status":"doing"}]
        })));
        assert_eq!(bad.code, ERR_BAD_ARGS);
        assert!(bad.output.contains("unknown status"));
    }
}
