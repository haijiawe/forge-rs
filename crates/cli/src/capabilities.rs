//! Fill the browser's capability snapshot from what is actually on disk.
//!
//! This lives in the CLI rather than in `hearth-server` for the reason stated in
//! `hearth_server::capabilities`: only this crate is allowed to know about
//! plugins, rosters, markets and skills. Everything here is a *read*: no child
//! process is started, no plugin is loaded, and a root that does not exist is
//! not an error — it just contributes no rows.
//!
//! The report is a snapshot taken at startup, not a live view. That is a
//! deliberate limit: plugins are loaded once, so what the model can reach does
//! not change during the life of the process. If that ever stops being true,
//! this must become a query instead of a snapshot.

use std::path::{Path, PathBuf};

use hearth_config::Config;
use hearth_server::{Entry, Report};

/// What the report needs to know about the session being served.
pub struct Facts<'a> {
    pub mode: &'a str,
    pub workspace: &'a str,
    pub model: &'a str,
    /// Data directory; the market store and the default plugin root live here.
    pub data_dir: &'a Path,
}

/// The tools this build registers when nothing optional is configured.
///
/// `present` and `worktree` are in here because they are unconditional; the
/// network tools and the platform bridges are not, because they only exist when
/// an operator configured them. Calling this twice a second would be wasteful —
/// it builds the tool objects — so it is called once, at startup, while the
/// report is being assembled.
fn builtin_tools() -> Vec<Entry> {
    let default = hearth_tool::Options::new(hearth_tool::DefaultLimits);
    hearth_tool::names(&default)
        .into_iter()
        .map(|name| Entry::new(name.clone(), describe_tool(&name).to_string(), true))
        .collect()
}

/// One line about a builtin, in the vocabulary of the person reading the panel.
///
/// Returns a `&'static str` so every arm stays a literal: a `String` return type
/// would force `.to_string()` onto seventeen arms to serve a caller that already
/// owns the row it is building.
fn describe_tool(name: &str) -> &'static str {
    match name {
        "fs_read" => "读工作区里的文件",
        "fs_write" => "写工作区里的文件",
        "fs_edit" => "按精确匹配替换文件里的一段文字",
        "fs_glob" => "按通配符找文件",
        "fs_grep" => "按正则搜索文件内容",
        "read_image" => "把一张图片读进对话（只在沙箱允许读它的路径范围内）",
        "shell" => "在工作区里跑一条命令，输出受截断限制",
        "worktree" => "在 git 里建、列、删工作树",
        "present" => "把一个文件交给操作者（登记路径，不改内容）",
        "todo_write" => "维护本轮的任务清单",
        "web_fetch" => "抓一个 URL 并抽成文本",
        "web_search" => "向已配置的搜索端点查询",
        "skill" => "按需读取一个技能",
        "subagent" => "派出一个子代理",
        "expert" => "召唤名册里的一个专家",
        "team" => "在一块工作板上并行跑一个团队",
        "goal_end" => "声明目标的裁决（完成或受阻）",
        // A tool added later would otherwise show an empty line, and an empty
        // line reads like "nothing to say" rather than "not described yet".
        _ => "这个构建注册的工具；没有为它写说明",
    }
}

/// Build the snapshot. Every section is best-effort: a broken roster yields the
/// rows it could read plus a diagnostic row, never an error, because the browser
/// panel must not be the reason a server refuses to start.
pub fn report(cfg: &Config, facts: &Facts<'_>) -> Report {
    let mut report = Report {
        mode: facts.mode.to_string(),
        workspace: facts.workspace.to_string(),
        model: facts.model.to_string(),
        ..Default::default()
    };

    // The tool list first, because it is the part no one else can fill: building
    // the tool objects is this crate's job, and `hearth-server` sits a layer
    // below `hearth-tool`. The event types and the route table are *not* set
    // here -- `Server::capability_report` fills those two from the build itself,
    // so there is one place that can be wrong instead of two.
    report.tools = builtin_tools();

    // Plugins: the interesting row is the installed-but-disabled one, so scan
    // both roots and mark rows active by comparing against `plugins.enabled`.
    let enabled: Vec<String> = cfg
        .plugins
        .enabled
        .iter()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .collect();
    let mut host = hearth_plugin::Host::start(Path::new(&cfg.plugin_root()));
    let bundles = facts.data_dir.join("market").join("bundles");
    if bundles.is_dir() {
        host.add_root(&bundles);
    }
    let (found, broken) = host.discover();
    for manifest in found {
        let active = enabled.iter().any(|name| name == &manifest.name);
        let detail = format!(
            "{} {}（{} 运行时）{}",
            manifest.name,
            manifest.version,
            manifest.runtime.as_str(),
            if active { "" } else { "，未启用" }
        );
        report
            .plugins
            .push(Entry::new(manifest.name, detail, active));
    }
    for (path, error) in broken {
        report.plugins.push(Entry::inactive(
            path.display().to_string(),
            format!("清单读不出来：{}", error.message()),
        ));
    }

    // Rosters: one scan per root, first root with experts wins, matching what
    // `wiring` does -- the panel must not claim a roster the session cannot use.
    let roots: Vec<PathBuf> = cfg
        .expert_roots()
        .iter()
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
        .collect();
    let mut roster = hearth_experts::Set::empty();
    for root in &roots {
        match hearth_experts::scan(root) {
            Ok(found) if !found.is_empty() => {
                roster = found;
                break;
            }
            Ok(_) => {}
            Err(error) => report.experts.push(Entry::inactive(
                root.display().to_string(),
                format!("名册读不出来：{}", error.message()),
            )),
        }
    }
    for expert in roster.experts() {
        let detail = if expert.description.is_empty() {
            expert.division.clone().unwrap_or_default()
        } else {
            expert.description.clone()
        };
        report.experts.push(Entry::new(
            expert.slug.clone(),
            detail,
            expert.enabled && roster.summonable(&expert.slug).is_some(),
        ));
    }
    for team in roster.teams() {
        report.teams.push(Entry::new(
            team.slug.clone(),
            format!(
                "{} 名成员，协调者是 {}",
                team.members.len(),
                team.coordinator
            ),
            // A team is reachable only when the tool is registered at all.
            cfg.team.max_parallel > 0,
        ));
    }

    // Bundles: a market install is a plugin the host already knows how to find,
    // so these rows say *where* it came from rather than duplicating the plugin.
    let store = hearth_market::Store::new(facts.data_dir.join("market"));
    match store.list() {
        Ok(installed) => {
            for record in installed {
                let name = record.name.clone();
                report.bundles.push(Entry::new(
                    name.clone(),
                    format!("{}，安装于 {}", record.version, record.installed_at),
                    enabled.iter().any(|item| item == &name),
                ));
            }
        }
        Err(error) => report.bundles.push(Entry::inactive(
            "market",
            format!("已安装清单读不出来：{}", error.message()),
        )),
    }

    // Skills: always reachable when present, because the scan is the source of
    // truth for the `skill` tool rather than a separate enable list.
    let skill_roots: Vec<PathBuf> = cfg
        .skill_roots()
        .iter()
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
        .collect();
    match hearth_skill::scan(&skill_roots) {
        Ok(set) => {
            for skill in set.skills.iter() {
                report.skills.push(Entry::new(
                    skill.name.clone(),
                    if skill.description.is_empty() {
                        skill.dir.clone()
                    } else {
                        skill.description.clone()
                    },
                    true,
                ));
            }
        }
        Err(error) => report.skills.push(Entry::inactive(
            "skills",
            format!("技能根目录读不出来：{}", error.message()),
        )),
    }

    // MCP: configuration only. Whether a server answers is a question for the
    // wiring notes, which have already tried and seen the answer.
    for server in &cfg.mcp.servers {
        let transport = if !server.transport.trim().is_empty() {
            server.transport.trim().to_string()
        } else if !server.url.trim().is_empty() {
            hearth_config::MCP_TRANSPORT_HTTP.to_string()
        } else {
            hearth_config::MCP_TRANSPORT_STDIO.to_string()
        };
        let detail = if transport == hearth_config::MCP_TRANSPORT_HTTP {
            server.url.clone()
        } else {
            server.command.join(" ")
        };
        report
            .mcp
            .push(Entry::new(server.name.clone(), detail, true));
    }

    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts<'a>(data_dir: &'a Path, workspace: &'a str) -> Facts<'a> {
        Facts {
            mode: "workspace-write",
            workspace,
            model: "test-model",
            data_dir,
        }
    }

    fn scratch(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hearth-caps-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        dir
    }

    #[test]
    fn a_bare_configuration_reports_named_but_empty_groups() {
        let dir = scratch("bare");
        let mut cfg = Config::defaults();
        cfg.data_dir = dir.display().to_string();
        cfg.plugins.enabled.clear();
        let report = report(&cfg, &facts(&dir, "C:/work"));

        assert_eq!(report.mode, "workspace-write");
        assert_eq!(report.model, "test-model");
        assert!(report.plugins.is_empty());
        assert!(report.experts.is_empty());
        assert!(report.teams.is_empty());
        assert!(report.bundles.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_installed_plugin_that_is_not_enabled_is_listed_as_inactive() {
        let dir = scratch("plugin");
        let plugin = dir.join("plugins").join("counter");
        std::fs::create_dir_all(&plugin).expect("plugin dir");
        std::fs::write(
            plugin.join("plugin.json"),
            r#"{"name":"counter","version":"1.0.0","description":"d","runtime":"node","entry":"index.js"}"#,
        )
        .expect("manifest");
        std::fs::write(plugin.join("index.js"), "\n").expect("entry");

        let mut cfg = Config::defaults();
        cfg.data_dir = dir.display().to_string();
        cfg.plugins.enabled.clear();

        let report = report(&cfg, &facts(&dir, "C:/work"));
        let row = report
            .plugins
            .iter()
            .find(|entry| entry.name == "counter")
            .expect("the installed plugin is named even though it is off");
        assert!(!row.active, "an unenabled plugin must not read as active");
        assert!(row.detail.contains("未启用"), "{}", row.detail);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn enabling_the_plugin_flips_the_same_row_to_active() {
        let dir = scratch("enabled");
        let plugin = dir.join("plugins").join("counter");
        std::fs::create_dir_all(&plugin).expect("plugin dir");
        std::fs::write(
            plugin.join("plugin.json"),
            r#"{"name":"counter","version":"1.0.0","description":"d","runtime":"node","entry":"index.js"}"#,
        )
        .expect("manifest");
        std::fs::write(plugin.join("index.js"), "\n").expect("entry");

        let mut cfg = Config::defaults();
        cfg.data_dir = dir.display().to_string();
        cfg.plugins.enabled = vec!["counter".to_string()];

        let report = report(&cfg, &facts(&dir, "C:/work"));
        let row = report
            .plugins
            .iter()
            .find(|entry| entry.name == "counter")
            .expect("row");
        assert!(row.active);
        assert!(!row.detail.contains("未启用"), "{}", row.detail);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_report_names_every_builtin_tool_with_a_line() {
        let dir = scratch("tools");
        let mut cfg = Config::defaults();
        cfg.data_dir = dir.display().to_string();
        let report = report(&cfg, &facts(&dir, "C:/work"));

        let names: Vec<&str> = report.tools.iter().map(|row| row.name.as_str()).collect();
        for expected in [
            "fs_read",
            "fs_write",
            "fs_edit",
            "fs_glob",
            "fs_grep",
            "read_image",
            "shell",
            "worktree",
            "present",
            "todo_write",
        ] {
            assert!(
                names.contains(&expected),
                "the report does not name {expected}; it names {names:?}"
            );
        }
        for row in &report.tools {
            assert!(
                !row.detail.trim().is_empty(),
                "{} has no detail, and a row without one is a row an agent cannot act on",
                row.name
            );
            assert!(row.active, "a builtin is registered, so it is active");
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
