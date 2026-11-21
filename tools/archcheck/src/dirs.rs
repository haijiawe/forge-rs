//! 每个装着东西的目录都得有一份说明书。
//!
//! 这条规则来自一次真实的抱怨：一个 agent（或一个人）`ls` 完仓库，能看见
//! `crates/`、`tests/`、`tools/` 这些名字，却看不出它们是干什么的——因为目录名
//! 只能表达「类别」，不能表达「这里放什么、不放什么、新东西该放哪一格」。
//! 一个目录里十几个子目录，缺的从来不是名字，而是那一页把它解释清楚的文字。
//!
//! 判据写在 `architecture.yaml` 的 `documented_dirs` 里，不写在这个文件里。
//! 理由和 `docs.rs` 相同：任何第二份名单都是一份会漂移的副本。哪一天新增了一个
//! 顶层目录，改动应该是 `architecture.yaml` 里加一行——那一行会被人看见，
//! 而藏在检查器源码里的数组不会。
//!
//! 边界也是刻意的。这条规则只检查**被声明的**容器目录，不扫描整棵树：
//! `crates/core/src/` 需要一个 README 吗？不需要，那里的组织方式由 crate
//! 自己的 README 负责解释。一条把每个 `src/` 都要求配说明的规则，
//! 只会在两天内被一批空洞的 README 喂饱。

use std::fs;
use std::path::Path;

use crate::report::Finding;

/// 说明书的名字。大写是刻意的：它要在 `ls` 里被一眼认出来。
const DOC: &str = "README.md";

pub fn check(root: &Path, declared: &[(String, usize)]) -> Vec<Finding> {
    let mut findings = Vec::new();

    for (relative, line) in declared {
        let dir = root.join(relative);
        if !dir.is_dir() {
            findings.push(Finding::error(
                "architecture.yaml",
                *line,
                "dirs.unknown",
                format!("documented_dirs names {relative}, which is not a directory on disk"),
            ));
            continue;
        }

        let readme = dir.join(DOC);
        let text = match fs::read_to_string(&readme) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                findings.push(Finding::error(
                    format!("{relative}/{DOC}"),
                    1,
                    "dirs.missing",
                    format!(
                        "{relative} holds tracked content but has no {DOC}; a reader arriving here has only the directory names to go on"
                    ),
                ));
                continue;
            }
            Err(err) => {
                findings.push(Finding::error(
                    format!("{relative}/{DOC}"),
                    1,
                    "dirs.unreadable",
                    format!("{relative}/{DOC} cannot be read: {err}"),
                ));
                continue;
            }
        };

        // 一份只有标题的说明书记录不了任何东西，而它会让这条规则变绿。
        // 门槛定得很低——一个标题加两行内容——因为这条规则要拦的是占位文件，
        // 不是判卷：写得够不够好是评审的事，写没写是这里的事。
        if !has_substance(&text) {
            findings.push(Finding::error(
                format!("{relative}/{DOC}"),
                1,
                "dirs.empty",
                format!(
                    "{relative}/{DOC} is a placeholder: it needs a heading and a few lines that say what {relative} holds"
                ),
            ));
        }
    }

    findings
}

fn has_substance(text: &str) -> bool {
    let mut headings = 0;
    let mut lines = 0;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        lines += 1;
        if trimmed.starts_with('#') {
            headings += 1;
        }
    }
    headings >= 1 && lines >= 3
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scratch(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("archcheck-dirs-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create fixture root");
        root
    }

    fn declared(path: &str) -> Vec<(String, usize)> {
        vec![(path.to_string(), 7)]
    }

    #[test]
    fn a_documented_directory_passes() {
        let root = scratch("ok");
        fs::create_dir_all(root.join("docs")).expect("create docs");
        fs::write(
            root.join("docs/README.md"),
            "# 文档\n\n这里是索引。\n\n## 决策\n\n见下一节。\n",
        )
        .expect("write readme");
        let findings = check(&root, &declared("docs"));
        let _ = fs::remove_dir_all(&root);
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn a_directory_without_a_readme_is_reported() {
        let root = scratch("missing");
        fs::create_dir_all(root.join("tools")).expect("create tools");
        let findings = check(&root, &declared("tools"));
        let _ = fs::remove_dir_all(&root);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].rule, "dirs.missing");
        assert_eq!(findings[0].file, "tools/README.md");
    }

    #[test]
    fn a_placeholder_readme_is_not_a_document() {
        let root = scratch("empty");
        fs::create_dir_all(root.join("bin")).expect("create bin");
        fs::write(root.join("bin/README.md"), "# bin\n").expect("write readme");
        let findings = check(&root, &declared("bin"));
        let _ = fs::remove_dir_all(&root);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].rule, "dirs.empty");
    }

    #[test]
    fn a_declared_directory_that_does_not_exist_is_reported_at_the_declaration() {
        let root = scratch("unknown");
        let findings = check(&root, &declared("nowhere"));
        let _ = fs::remove_dir_all(&root);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].rule, "dirs.unknown");
        assert_eq!(findings[0].file, "architecture.yaml");
        assert_eq!(findings[0].line, 7);
    }
}
