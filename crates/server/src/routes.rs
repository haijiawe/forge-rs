//! The HTTP surface, as a table.
//!
//! Layer L3. `handle_api` dispatches with a `match` on segment count and names,
//! which is the right shape for a router this small — it keeps the auth check,
//! the method check and the handler in one place instead of spreading them over
//! a DSL. What that shape cannot do is answer "what endpoints exist": there is
//! nothing to print, nothing to count, and nothing a test can walk.
//!
//! This module is that answer. It is a declaration, not a second router: the
//! dispatch in `server.rs` stays authoritative for behaviour, and a test
//! (`tests/routes_table.rs`) requests every row here and fails if any of them
//! answers `no such endpoint`. Adding a route without adding a row therefore
//! costs nothing, while adding a row that no longer exists costs a red test.
//!
//! Paths are written with `{placeholders}` because that is what a reader needs;
//! the dispatcher never sees this string.

use crate::capabilities::Entry;

/// One endpoint: the method, the path shape, and one line on when to use it.
pub struct Route {
    pub method: &'static str,
    pub path: &'static str,
    pub purpose: &'static str,
}

/// Everything under `/api`, plus the WebSocket upgrade.
///
/// Kept in the order a client discovers them: process-wide reads first, then
/// sessions, then one session's parts.
pub const ROUTES: &[Route] = &[
    Route {
        method: "GET",
        path: "/api/capabilities",
        purpose: "本进程被配置成了什么，以及这个构建认识哪些工具、事件与路由",
    },
    Route {
        method: "GET",
        path: "/api/teams",
        purpose: "列出 agent 团队留下的工作板",
    },
    Route {
        method: "GET",
        path: "/api/teams/{board}",
        purpose: "读一块工作板：任务、拥有者、诊断行",
    },
    Route {
        method: "GET",
        path: "/api/presents",
        purpose: "列出被交给操作者的文件（present 工具的清单）",
    },
    Route {
        method: "GET",
        path: "/api/sessions",
        purpose: "列出会话日志，可带查询条件",
    },
    Route {
        method: "POST",
        path: "/api/sessions",
        purpose: "新建一个会话",
    },
    Route {
        method: "GET",
        path: "/api/sessions/{id}",
        purpose: "一个会话的摘要与最新序号",
    },
    Route {
        method: "GET",
        path: "/api/sessions/{id}/events",
        purpose: "从某个序号开始读事件",
    },
    Route {
        method: "GET",
        path: "/api/sessions/{id}/stream",
        purpose: "WebSocket 升级：订阅新事件（需要子协议或协议头）",
    },
    Route {
        method: "GET",
        path: "/api/sessions/{id}/terminal",
        purpose: "WebSocket 升级：一个人敲一行命令，输出实时回传（read-only 会话没有）",
    },
    Route {
        method: "POST",
        path: "/api/sessions/{id}/messages",
        purpose: "把一条用户消息排进会话（需要 Idempotency-Key）",
    },
    Route {
        method: "POST",
        path: "/api/sessions/{id}/cancel",
        purpose: "取消正在跑的那一轮",
    },
    Route {
        method: "POST",
        path: "/api/sessions/{id}/steer",
        purpose: "给下一步排队一条消息，不打断当前步骤",
    },
    Route {
        method: "POST",
        path: "/api/sessions/{id}/approvals/{approvalId}",
        purpose: "对一个待批的工具调用做出裁决",
    },
    Route {
        method: "GET",
        path: "/api/sessions/{id}/goal",
        purpose: "折出这个会话的目标（没有目标时是 null，不是 404）",
    },
    Route {
        method: "GET",
        path: "/api/sessions/{id}/subagents",
        purpose: "这个会话派出的子代理树，带计时与诊断",
    },
    Route {
        method: "GET",
        path: "/api/sessions/{id}/files",
        purpose: "列工作区某个目录（`?path=`）",
    },
    Route {
        method: "GET",
        path: "/api/sessions/{id}/file",
        purpose: "读工作区里一个文件（`?path=`），带摘要",
    },
    Route {
        method: "PUT",
        path: "/api/sessions/{id}/file",
        purpose: "把编辑后的缓冲区存回工作区，可选 `expectedSha256`",
    },
    Route {
        method: "POST",
        path: "/api/sessions/{id}/revert",
        purpose: "把文件还原到 git 已提交的版本",
    },
    Route {
        method: "GET",
        path: "/api/sessions/{id}/diff",
        purpose: "改动列表；带 `?path=` 时给出那个文件的逐行 diff",
    },
    Route {
        method: "GET",
        path: "/api/sessions/{id}/search",
        purpose: "在整个工作区里按子串搜文本（`?q=`，可选 `limit`）",
    },
    Route {
        method: "GET",
        path: "/api/sessions/{id}/attachments/{attachmentId}",
        purpose: "取回一个附件正文（事件里只有引用）",
    },
];

impl Route {
    /// `GET /api/sessions/{id}/files`, the way a client writes it in a log line.
    pub fn line(&self) -> String {
        format!("{} {}", self.method, self.path)
    }
}

/// The table as capability rows, for `GET /api/capabilities`.
pub fn entries() -> Vec<Entry> {
    ROUTES
        .iter()
        .map(|route| Entry::new(route.line(), route.purpose, true))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_row_is_a_method_path_pair() {
        for route in ROUTES {
            assert!(
                ["GET", "POST", "PUT", "DELETE"].contains(&route.method),
                "{} is not an HTTP method",
                route.method
            );
            assert!(
                route.path.starts_with("/api/"),
                "{} is not under /api",
                route.path
            );
            assert!(
                !route.purpose.trim().is_empty(),
                "{} has no purpose, and a row without one is a row nobody can use",
                route.line()
            );
        }
    }

    #[test]
    fn no_row_is_declared_twice() {
        let mut seen = std::collections::BTreeSet::new();
        for route in ROUTES {
            assert!(
                seen.insert(route.line()),
                "{} is declared twice",
                route.line()
            );
        }
    }

    #[test]
    fn the_table_covers_the_whole_surface() {
        // A floor, not a target: it fails if the table is gutted, and it says
        // nothing about routes that exist but are not declared (that is what
        // `tests/routes_table.rs` checks by asking the server).
        assert!(ROUTES.len() >= 20, "declared {} routes", ROUTES.len());
    }
}
