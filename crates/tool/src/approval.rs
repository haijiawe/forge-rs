//! Policy and approval boundaries, ported from `archive/forge-go/internal/tool/approval.go`.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::Call;

/// What a policy says about one call. The default is deny: a policy that fails
/// to classify a call must not fall through to permission.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    #[default]
    Deny,
    Ask,
    Allow,
}

impl Decision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Deny => "deny",
            Self::Ask => "ask",
            Self::Allow => "allow",
        }
    }
}

impl fmt::Display for Decision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Pure, fast classification of a call. No filesystem reads, no side effects.
pub trait Policy: Send + Sync {
    fn decide(&self, call: &Call) -> (Decision, String);
}

/// The question presented to a human. The loop records the request; the answer
/// belongs to whoever produced it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub call: Call,
    pub reason: String,
}

/// Asks a human. `None` in the executor means no human is available, so every
/// `Decision::Ask` is refused. Errors are messages, not tool outcomes.
pub trait Approver: Send + Sync {
    fn approve(&self, request: ApprovalRequest) -> std::result::Result<bool, String>;

    /// True when this approver writes its own approval/decision record.
    fn records_own_decision(&self) -> bool {
        false
    }
}

/// Marker for an approver that implements `Approver::records_own_decision`.
pub trait DecisionRecorder: Approver {}

/// Adapt a function into a policy.
pub struct PolicyFunc(fn(&Call) -> (Decision, String));

impl PolicyFunc {
    pub fn new(f: fn(&Call) -> (Decision, String)) -> Self {
        Self(f)
    }
}

impl Policy for PolicyFunc {
    fn decide(&self, call: &Call) -> (Decision, String) {
        (self.0)(call)
    }
}

/// Explicit allow policy for tests and embedders; the CLI never uses it.
#[allow(non_upper_case_globals)]
pub const AllowAll: PolicyFunc = PolicyFunc(|_| (Decision::Allow, "no policy".to_string()));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_decision_is_deny() {
        assert_eq!(Decision::default(), Decision::Deny);
        assert_eq!(Decision::Ask.to_string(), "ask");
    }

    #[test]
    fn allow_all_reports_its_reason() {
        let call = Call::new("c", "shell", serde_json::Value::Null, ".");
        assert_eq!(
            AllowAll.decide(&call),
            (Decision::Allow, "no policy".into())
        );
    }
}
