//! The single choke point between a model call and a tool side effect.
//! Ported from `archive/forge-go/internal/tool/executor.go`.

use std::panic::{self, AssertUnwindSafe};
use std::sync::Arc;
use std::time::Instant;

use forge_llm::ToolSpec;
use serde_json::json;

use crate::approval::{ApprovalRequest, Approver, Policy};
use crate::contract::{Call, Result, CODE_DENIED};
use crate::truncate::{effective_limits, truncate, Limits};
use crate::Registry;

pub const CODE_UNKNOWN_TOOL: &str = "unknown_tool";
pub const CODE_PANIC: &str = "panic";
pub const CODE_APPROVAL_FAILED: &str = "approval_failed";
pub const CODE_DENIED_REFUSED: &str = "denied_refused";
pub const CODE_NO_APPROVER: &str = "no_approver";

#[derive(Clone, Default)]
pub struct ExecutorOptions {
    /// No policy means allow with reason `no policy configured` (test use only).
    pub policy: Option<Arc<dyn Policy>>,
    /// No approver refuses every ask; unattended consent is not consent.
    pub approver: Option<Arc<dyn Approver>>,
    pub limits: Limits,
    pub workspace: String,
}

#[derive(Clone)]
pub struct Executor {
    registry: Option<Arc<Registry>>,
    options: ExecutorOptions,
}

impl Executor {
    pub fn new(registry: Arc<Registry>, options: ExecutorOptions) -> Self {
        Self {
            registry: Some(registry),
            options,
        }
    }

    pub fn from_registry(registry: Registry, options: ExecutorOptions) -> Self {
        Self::new(Arc::new(registry), options)
    }

    /// Run one call through defaults, lookup, policy, approval, execution, and
    /// bounding. No events are written; the loop owns the log.
    pub fn run(&self, mut call: Call) -> Result {
        let started = Instant::now();
        let limits = effective_limits(self.options.limits);
        if call.workspace.is_empty() {
            call.workspace = self.options.workspace.clone();
        }

        let mut policy_ran = false;
        let mut policy_reason = String::new();
        let mut result = self.admit_and_run(&call, limits, &mut policy_ran, &mut policy_reason);

        let (output, cut) = truncate(&result.output, limits);
        result.output = output;
        result.truncated = result.truncated || cut;

        if policy_ran {
            result
                .meta
                .insert("policy".to_string(), json!(policy_reason));
        }
        result.meta.insert(
            "durationMs".to_string(),
            json!(started.elapsed().as_millis() as i64),
        );
        result
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.registry
            .as_ref()
            .map(|registry| registry.specs())
            .unwrap_or_default()
    }

    pub fn workspace(&self) -> &str {
        &self.options.workspace
    }

    fn admit_and_run(
        &self,
        call: &Call,
        _limits: Limits,
        policy_ran: &mut bool,
        policy_reason: &mut String,
    ) -> Result {
        let registry = match &self.registry {
            Some(registry) => registry,
            None => return unknown_tool(call, &Registry::default()),
        };
        let Some(tool) = registry.lookup(&call.name) else {
            return unknown_tool(call, registry);
        };

        *policy_ran = true;
        let decision = match &self.options.policy {
            None => {
                *policy_reason = "no policy configured".to_string();
                crate::Decision::Allow
            }
            Some(policy) => {
                let (decision, reason) = policy.decide(call);
                *policy_reason = reason;
                decision
            }
        };

        match decision {
            crate::Decision::Allow => {}
            crate::Decision::Deny => {
                return Result::error(
                    CODE_DENIED,
                    format!(
                        "call to {:?} denied by policy{}",
                        call.name,
                        reason_suffix(policy_reason)
                    ),
                )
            }
            crate::Decision::Ask => {
                let approver = match &self.options.approver {
                    Some(approver) => approver,
                    None => {
                        return Result::error(
                            CODE_NO_APPROVER,
                            "approval required but no approver is available",
                        )
                    }
                };
                let request = ApprovalRequest {
                    call: call.clone(),
                    reason: policy_reason.clone(),
                };
                match approver.approve(request) {
                    Ok(true) => {}
                    Ok(false) => {
                        return Result::error(
                            CODE_DENIED_REFUSED,
                            format!(
                                "call to {:?}: approval refused{}",
                                call.name,
                                reason_suffix(policy_reason)
                            ),
                        )
                    }
                    Err(err) => {
                        return Result::error(
                            CODE_APPROVAL_FAILED,
                            format!("call to {:?}: approval failed: {err}", call.name),
                        )
                    }
                }
            }
        }

        match panic::catch_unwind(AssertUnwindSafe(|| tool.call(call.clone()))) {
            Ok(result) => result,
            Err(payload) => Result::error(
                CODE_PANIC,
                format!("tool {:?} panicked: {}", call.name, panic_text(&payload)),
            ),
        }
    }
}

fn unknown_tool(call: &Call, registry: &Registry) -> Result {
    let names = registry.names();
    let available = if names.is_empty() {
        "no tools are registered".to_string()
    } else {
        format!("available tools: {}", names.join(", "))
    };
    Result::error(
        CODE_UNKNOWN_TOOL,
        format!("unknown tool {:?}; {available}", call.name),
    )
}

fn reason_suffix(reason: &str) -> String {
    if reason.is_empty() {
        String::new()
    } else {
        format!(": {reason}")
    }
}

fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_string()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "<non-string panic>".to_string()
    }
}
