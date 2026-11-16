//! Skill discovery and on-demand loading.
//!
//! The scanner looks exactly one level below each root: `root/*/SKILL.md`.
//! It never writes, never recurses into a skill directory, and never reads a
//! body until [`Skill::load`] is called. Diagnostics are data, not errors:
//! [`Set::skipped`] and [`Set::warnings`] are sorted so goldens are stable.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use crate::error::{Error, ErrorKind, Result};
use crate::frontmatter::parse_frontmatter;
use crate::fsio::{read_capped, Entry, Fs, Meta, RealFs, SKILL_FILE_NAME};
use crate::util::{clean_path, describe_io, display, quote};

pub use crate::error::{Error as SkillError, ErrorKind as SkillErrorKind};

/// One discovered skill. `body` is empty until [`Skill::load`] is called.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skill {
    /// Skill name from frontmatter, or the directory name as a fallback.
    pub name: String,
    /// Description from frontmatter; empty when there is no frontmatter.
    pub description: String,
    /// Skill directory, normalised to forward slashes.
    pub dir: String,
    /// Path of `SKILL.md`, normalised to forward slashes.
    pub path: String,
    /// File body, loaded only by [`Skill::load`].
    pub body: String,
    /// Sorted relative paths of sibling regular files, excluding `SKILL.md`.
    pub files: Vec<String>,
}

impl Skill {
    /// Return a copy of this skill with `body` filled from disk.
    ///
    /// The receiver is never mutated, and every frontmatter warning is
    /// ignored here: scanning already reported it. Only an unreadable file,
    /// unparseable frontmatter, or an empty path is an error.
    pub fn load(&self) -> Result<Skill> {
        if self.path.trim().is_empty() {
            return Err(Error::new(ErrorKind::NoPath, "skill: skill has no path"));
        }
        let path = Path::new(&self.path);
        let (data, _) = read_capped(&RealFs, path).map_err(|err| load_error(&self.path, &err))?;
        let parsed = parse_frontmatter(&data).map_err(|err| load_error(&self.path, &err))?;
        let mut loaded = self.clone();
        loaded.body = parsed.body;
        Ok(loaded)
    }
}

/// Everything one or more scans found, in deterministic byte order.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Set {
    /// Accepted skills, sorted by name then path.
    pub skills: Vec<Skill>,
    /// Fatal-per-entry diagnostics, sorted.
    pub skipped: Vec<String>,
    /// Non-fatal diagnostics, sorted.
    pub warnings: Vec<String>,
}

/// Scan `roots` for `root/*/SKILL.md` skills.
///
/// Filesystem problems are reported in [`Set::skipped`] / [`Set::warnings`];
/// this function itself only fails if an internal invariant is broken.
pub fn scan(roots: &[PathBuf]) -> Result<Set> {
    scan_with(&RealFs, roots)
}

/// Contract spelling of [`scan`] for callers porting the Go API verbatim.
#[allow(non_snake_case)]
pub fn Scan(roots: &[PathBuf]) -> Result<Set> {
    scan(roots)
}

fn load_error(path: &str, err: &Error) -> Error {
    Error::new(
        err.kind(),
        format!("skill: load {}: {}", path, err.message()),
    )
}

/// The filesystem seam is injected only by unit tests.
fn scan_with(fs: &dyn Fs, roots: &[PathBuf]) -> Result<Set> {
    let mut set = Set::default();
    let mut providers: BTreeMap<String, String> = BTreeMap::new();
    let mut visited: BTreeMap<String, String> = BTreeMap::new();
    for root in roots {
        scan_root(fs, root, &mut set, &mut providers, &mut visited);
    }
    sort_set(&mut set);
    Ok(set)
}

fn scan_root(
    fs: &dyn Fs,
    root: &Path,
    set: &mut Set,
    providers: &mut BTreeMap<String, String>,
    visited: &mut BTreeMap<String, String>,
) {
    if root.as_os_str().is_empty() || root.to_string_lossy().trim().is_empty() {
        set.warnings
            .push("\"\": root path is empty; skipped".to_string());
        return;
    }
    let root_text = display(root);
    match fs.metadata(root) {
        Ok(meta) if meta.is_dir => {}
        Ok(_) => {
            set.warnings
                .push(format!("{root_text}: root is not a directory; skipped"));
            return;
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            set.warnings
                .push(format!("{root_text}: root does not exist (yet); skipped"));
            return;
        }
        Err(err) => {
            set.warnings.push(format!(
                "{root_text}: root is not usable: {}; skipped",
                describe_io(&err)
            ));
            return;
        }
    }

    let mut entries = match fs.read_dir(root) {
        Ok(entries) => entries,
        Err(err) => {
            set.warnings.push(format!(
                "{root_text}: cannot list root: {}; skipped",
                describe_io(&err)
            ));
            return;
        }
    };
    entries.sort_by(|left, right| left.name.cmp(&right.name));

    let mut dirs = Vec::new();
    for entry in entries {
        let dir = root.join(&entry.name);
        match fs.metadata(&dir) {
            Ok(meta) if meta.is_dir => dirs.push(dir),
            Ok(_) => {}
            Err(err) => set.warnings.push(format!(
                "{}: cannot inspect entry: {}; skipped",
                display(&dir),
                describe_io(&err)
            )),
        }
    }
    dirs.sort_by_key(|left| display(left));

    for dir in dirs {
        let key = real_key(fs, &dir);
        let dir_text = display(&dir);
        if let Some(first) = visited.get(&key) {
            set.warnings.push(format!(
                "{dir_text}: same directory as {first} (symlink loop or repeated root); skipped"
            ));
            continue;
        }
        visited.insert(key, dir_text.clone());
        scan_dir(fs, &dir_text, set, providers);
    }
}

fn scan_dir(fs: &dyn Fs, dir: &str, set: &mut Set, providers: &mut BTreeMap<String, String>) {
    let path = format!("{dir}/{SKILL_FILE_NAME}");
    let skill_path = Path::new(&path);
    match fs.metadata(skill_path) {
        Ok(meta) if meta.is_dir => {
            set.skipped.push(format!("{dir}: SKILL.md is a directory"));
            return;
        }
        Ok(_) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            set.skipped.push(format!("{dir}: no SKILL.md"));
            return;
        }
        Err(err) => {
            set.warnings.push(format!(
                "{path}: cannot inspect SKILL.md: {}",
                describe_io(&err)
            ));
            return;
        }
    }

    let (data, truncated) = match read_capped(fs, skill_path) {
        Ok(result) => result,
        Err(err) => {
            set.warnings
                .push(format!("{path}: SKILL.md is unreadable: {}", err.message()));
            return;
        }
    };
    let parsed = match parse_frontmatter(&data) {
        Ok(parsed) => parsed,
        Err(err) => {
            set.skipped.push(format!("{dir}: {}", err.message()));
            return;
        }
    };
    for warning in &parsed.warnings {
        set.warnings.push(format!("{path}: {warning}"));
    }
    if truncated {
        set.warnings.push(format!(
            "{path}: larger than {} bytes; body truncated",
            1 << 20
        ));
    }

    let mut name = parsed
        .fields
        .get("name")
        .map(|value| value.trim().to_string())
        .unwrap_or_default();
    if name.is_empty() {
        name = Path::new(dir)
            .file_name()
            .map(|part| part.to_string_lossy().into_owned())
            .unwrap_or_else(|| dir.to_string());
    }
    if let Err(reason) = validate_name(&name) {
        set.skipped.push(format!("{dir}: {reason}"));
        return;
    }

    let description = parsed
        .fields
        .get("description")
        .map(|value| value.trim().to_string())
        .unwrap_or_default();
    if parsed.has_frontmatter && description.is_empty() {
        set.skipped
            .push(format!("{dir}: frontmatter has no description field"));
        return;
    }

    if let Some(first) = providers.get(&name) {
        set.skipped.push(format!(
            "{dir}: duplicate skill name {} (already provided by {first})",
            quote(&name)
        ));
        return;
    }

    let files = match list_files(fs, dir) {
        Ok(files) => files,
        Err(err) => {
            set.warnings
                .push(format!("{dir}: cannot list files: {}", describe_io(&err)));
            Vec::new()
        }
    };
    providers.insert(name.clone(), dir.to_string());
    set.skills.push(Skill {
        name,
        description,
        dir: dir.to_string(),
        path,
        body: String::new(),
        files,
    });
}

/// Only regular sibling files, relative and sorted; `SKILL.md` is excluded.
fn list_files(fs: &dyn Fs, dir: &str) -> io::Result<Vec<String>> {
    let mut files = Vec::new();
    for entry in fs.read_dir(Path::new(dir))? {
        let Entry {
            name,
            is_dir,
            is_symlink,
        } = entry;
        if name == SKILL_FILE_NAME || is_dir {
            continue;
        }
        if is_symlink {
            let full = format!("{dir}/{name}");
            match fs.metadata(Path::new(&full)) {
                Ok(Meta { is_dir: true }) | Err(_) => continue,
                Ok(_) => {}
            }
        }
        files.push(name);
    }
    files.sort();
    Ok(files)
}

fn real_key(fs: &dyn Fs, path: &Path) -> String {
    match fs.canonicalize(path) {
        Ok(resolved) => display(&resolved),
        Err(_) => clean_path(&display(path)),
    }
}

fn validate_name(name: &str) -> std::result::Result<(), String> {
    if name.is_empty() || name == "." || name == ".." {
        return Err(format!(
            "skill directory name {} cannot be used as a skill name",
            quote(name)
        ));
    }
    if name
        .chars()
        .any(|ch| ch == '/' || ch == '\\' || (ch as u32) < 0x20 || ch == '\u{7f}')
    {
        return Err(format!(
            "invalid skill name {} (no path separators or control characters)",
            quote(name)
        ));
    }
    Ok(())
}

fn sort_set(set: &mut Set) {
    set.skills.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.path.cmp(&right.path))
    });
    set.skipped.sort();
    set.warnings.sort();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // ---------- test seams ----------

    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    enum Seam {
        #[default]
        None,
        OpenSkill,
        StatRoot,
        ReadDirRoot,
        StatWeird,
        StatSkill,
        ReadDirNoFiles,
    }

    #[derive(Default)]
    struct FakeFs {
        metadata_errors: Vec<(String, io::ErrorKind)>,
        metadata_dirs: Vec<String>,
        read_dir_errors: Vec<(String, io::ErrorKind)>,
        read_dir_overrides: Vec<(String, Vec<Entry>)>,
        open_errors: Vec<(String, io::ErrorKind)>,
        canonicalize_errors: Vec<String>,
    }

    impl FakeFs {
        fn with_seam(seam: Seam, base: &Path) -> Self {
            let mut fs = Self::default();
            match seam {
                Seam::None => {}
                Seam::OpenSkill | Seam::StatSkill => {
                    fs.open_errors
                        .push(("/SKILL.md".to_string(), io::ErrorKind::PermissionDenied));
                    if seam == Seam::StatSkill {
                        fs.metadata_errors
                            .push(("/SKILL.md".to_string(), io::ErrorKind::PermissionDenied));
                    }
                }
                Seam::StatRoot => fs
                    .metadata_errors
                    .push((display(&base.join("root")), io::ErrorKind::PermissionDenied)),
                Seam::ReadDirRoot => fs
                    .read_dir_errors
                    .push((display(&base.join("root")), io::ErrorKind::PermissionDenied)),
                Seam::StatWeird => fs
                    .metadata_errors
                    .push(("/weird".to_string(), io::ErrorKind::PermissionDenied)),
                Seam::ReadDirNoFiles => fs
                    .read_dir_errors
                    .push(("/nofiles".to_string(), io::ErrorKind::PermissionDenied)),
            }
            fs
        }
    }

    impl Fs for FakeFs {
        fn metadata(&self, path: &Path) -> io::Result<Meta> {
            let text = display(path);
            for (suffix, kind) in &self.metadata_errors {
                if text.ends_with(suffix) {
                    return Err(io::Error::from(*kind));
                }
            }
            if self
                .metadata_dirs
                .iter()
                .any(|suffix| text.ends_with(suffix))
            {
                return Ok(Meta { is_dir: true });
            }
            RealFs.metadata(path)
        }

        fn read_dir(&self, path: &Path) -> io::Result<Vec<Entry>> {
            let text = display(path);
            for (target, entries) in &self.read_dir_overrides {
                if text == *target {
                    return Ok(entries.clone());
                }
            }
            for (suffix, kind) in &self.read_dir_errors {
                if text.ends_with(suffix) {
                    return Err(io::Error::from(*kind));
                }
            }
            RealFs.read_dir(path)
        }

        fn open(&self, path: &Path) -> io::Result<fs::File> {
            let text = display(path);
            for (suffix, kind) in &self.open_errors {
                if text.ends_with(suffix) {
                    return Err(io::Error::from(*kind));
                }
            }
            RealFs.open(path)
        }

        fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
            let text = display(path);
            if self.canonicalize_errors.iter().any(|s| text.ends_with(s)) {
                return Err(io::Error::from(io::ErrorKind::NotFound));
            }
            RealFs.canonicalize(path)
        }
    }

    static NEXT_TREE: AtomicUsize = AtomicUsize::new(0);

    struct TempTree {
        root: PathBuf,
    }

    impl TempTree {
        fn new(tag: &str) -> Self {
            let serial = NEXT_TREE.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir()
                .join(format!("forge-skill-{tag}-{}-{serial}", std::process::id()));
            fs::create_dir_all(&root).expect("create temp root");
            Self { root }
        }

        fn path(&self, relative: &str) -> PathBuf {
            let mut path = self.root.clone();
            for part in relative.split('/') {
                if !part.is_empty() {
                    path.push(part);
                }
            }
            path
        }

        fn mkdir(&self, relative: &str) {
            fs::create_dir_all(self.path(relative)).expect("mkdir");
        }

        fn write(&self, relative: &str, content: &str) {
            let path = self.path(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("parent dir");
            }
            fs::write(path, content).expect("write file");
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn valid(name: &str, description: &str) -> String {
        format!("---\nname: {name}\ndescription: {description}\n---\nbody\n")
    }

    fn expand(template: &str, base: &Path) -> String {
        template.replace("{base}", &display(base))
    }

    fn render_skill(skill: &Skill) -> String {
        format!(
            "{}: {} [{}]",
            skill.name,
            skill.description,
            skill.files.join(" ")
        )
    }

    fn assert_diagnostics(
        set: &Set,
        want_skipped: &[String],
        want_warnings: &[String],
        case: &str,
    ) {
        assert_eq!(set.skipped, want_skipped, "skipped, case: {case}");
        assert_eq!(set.warnings, want_warnings, "warnings, case: {case}");
    }

    fn assert_invariants(set: &Set) {
        for skill in &set.skills {
            assert!(skill.body.is_empty(), "body loaded during scan: {skill:?}");
            assert!(
                skill.files.windows(2).all(|pair| pair[0] <= pair[1]),
                "files unsorted: {skill:?}"
            );
        }
        assert!(set
            .skills
            .windows(2)
            .all(|pair| { (&pair[0].name, &pair[0].path) <= (&pair[1].name, &pair[1].path) }));
        assert!(set.skipped.windows(2).all(|pair| pair[0] <= pair[1]));
        assert!(set.warnings.windows(2).all(|pair| pair[0] <= pair[1]));
    }

    struct ScanCase {
        name: &'static str,
        files: Vec<(&'static str, String)>,
        dirs: Vec<&'static str>,
        roots: Vec<&'static str>,
        seam: Seam,
        want_skills: Vec<&'static str>,
        want_skipped: Vec<&'static str>,
        want_warnings: Vec<&'static str>,
    }

    fn cases() -> Vec<ScanCase> {
        vec![
            ScanCase {
                name: "basic with sorted files, subdirectory not listed",
                files: vec![
                    ("root/alpha/SKILL.md", valid("alpha", "An alpha skill")),
                    ("root/alpha/zeta.md", "z".to_string()),
                    ("root/alpha/mid.txt", "m".to_string()),
                    ("root/alpha/examples/deep/inner", "deep".to_string()),
                ],
                dirs: vec![],
                roots: vec!["root"],
                seam: Seam::None,
                want_skills: vec!["alpha: An alpha skill [mid.txt zeta.md]"],
                want_skipped: vec![],
                want_warnings: vec![],
            },
            ScanCase {
                name: "name falls back to directory",
                files: vec![(
                    "root/fallback/SKILL.md",
                    "---\ndescription: Described only\n---\nbody\n".to_string(),
                )],
                dirs: vec![],
                roots: vec!["root"],
                seam: Seam::None,
                want_skills: vec!["fallback: Described only []"],
                want_skipped: vec![],
                want_warnings: vec![],
            },
            ScanCase {
                name: "CRLF parses like LF",
                files: vec![(
                    "root/crlf/SKILL.md",
                    "---\r\nname: crlf\r\ndescription: CRLF\r\n---\r\nline one\r\nline two\r\n"
                        .to_string(),
                )],
                dirs: vec![],
                roots: vec!["root"],
                seam: Seam::None,
                want_skills: vec!["crlf: CRLF []"],
                want_skipped: vec![],
                want_warnings: vec![],
            },
            ScanCase {
                name: "no frontmatter still loads with warning",
                files: vec![("root/bare/SKILL.md", "just text".to_string())],
                dirs: vec![],
                roots: vec!["root"],
                seam: Seam::None,
                want_skills: vec!["bare:  []"],
                want_skipped: vec![],
                want_warnings: vec![
                    "{base}/root/bare/SKILL.md: no frontmatter; name falls back to the directory name",
                ],
            },
            ScanCase {
                name: "empty description skipped",
                files: vec![(
                    "root/nod/SKILL.md",
                    "---\nname: nod\ndescription: \"\"\n---\nbody\n".to_string(),
                )],
                dirs: vec![],
                roots: vec!["root"],
                seam: Seam::None,
                want_skills: vec![],
                want_skipped: vec!["{base}/root/nod: frontmatter has no description field"],
                want_warnings: vec![],
            },
            ScanCase {
                name: "blank description skipped",
                files: vec![(
                    "root/blank/SKILL.md",
                    "---\nname: blank\ndescription:   \n---\nbody\n".to_string(),
                )],
                dirs: vec![],
                roots: vec!["root"],
                seam: Seam::None,
                want_skills: vec![],
                want_skipped: vec!["{base}/root/blank: frontmatter has no description field"],
                want_warnings: vec![],
            },
            ScanCase {
                name: "unclosed frontmatter skipped",
                files: vec![(
                    "root/unclosed/SKILL.md",
                    "---\nname: unclosed\ndescription: beta\nno closing fence\n".to_string(),
                )],
                dirs: vec![],
                roots: vec!["root"],
                seam: Seam::None,
                want_skills: vec![],
                want_skipped: vec![
                    "{base}/root/unclosed: frontmatter is not closed by a second --- line",
                ],
                want_warnings: vec![],
            },
            ScanCase {
                name: "SKILL.md directory skipped",
                files: vec![],
                dirs: vec!["root/beta/SKILL.md"],
                roots: vec!["root"],
                seam: Seam::None,
                want_skills: vec![],
                want_skipped: vec!["{base}/root/beta: SKILL.md is a directory"],
                want_warnings: vec![],
            },
            ScanCase {
                name: "directory without SKILL.md skipped",
                files: vec![("root/empty/keep.txt", "x".to_string())],
                dirs: vec![],
                roots: vec!["root"],
                seam: Seam::None,
                want_skills: vec![],
                want_skipped: vec!["{base}/root/empty: no SKILL.md"],
                want_warnings: vec![],
            },
            ScanCase {
                name: "plain file in root is not a candidate",
                files: vec![
                    ("root/stray.md", "x".to_string()),
                    ("root/real/SKILL.md", valid("real", "Real")),
                ],
                dirs: vec![],
                roots: vec!["root"],
                seam: Seam::None,
                want_skills: vec!["real: Real []"],
                want_skipped: vec![],
                want_warnings: vec![],
            },
            ScanCase {
                name: "duplicate name across roots: first wins",
                files: vec![
                    ("r1/dup/SKILL.md", valid("dup", "First")),
                    ("r2/dup/SKILL.md", valid("dup", "Second")),
                ],
                dirs: vec![],
                roots: vec!["r1", "r2"],
                seam: Seam::None,
                want_skills: vec!["dup: First []"],
                want_skipped: vec![
                    "{base}/r2/dup: duplicate skill name \"dup\" (already provided by {base}/r1/dup)",
                ],
                want_warnings: vec![],
            },
            ScanCase {
                name: "horizontal rule in body does not truncate",
                files: vec![(
                    "root/dashes/SKILL.md",
                    "---\nname: dashes\ndescription: Horizontal rule\n---\nfirst\n---\nlast\n"
                        .to_string(),
                )],
                dirs: vec![],
                roots: vec!["root"],
                seam: Seam::None,
                want_skills: vec!["dashes: Horizontal rule []"],
                want_skipped: vec![],
                want_warnings: vec![],
            },
            ScanCase {
                name: "nested value dropped with warning",
                files: vec![(
                    "root/nested/SKILL.md",
                    "---\nname: nested\ndescription: Nested\ntags:\n  - one\n---\nbody\n"
                        .to_string(),
                )],
                dirs: vec![],
                roots: vec!["root"],
                seam: Seam::None,
                want_skills: vec!["nested: Nested []"],
                want_skipped: vec![],
                want_warnings: vec![
                    "{base}/root/nested/SKILL.md: line 5: \"tags\" has a nested or multi-line value, which is not supported; key dropped",
                ],
            },
            ScanCase {
                name: "block scalar description skipped",
                files: vec![(
                    "root/block/SKILL.md",
                    "---\nname: block\ndescription: >-\n  long text\n---\nbody\n".to_string(),
                )],
                dirs: vec![],
                roots: vec!["root"],
                seam: Seam::None,
                want_skills: vec![],
                want_skipped: vec!["{base}/root/block: frontmatter has no description field"],
                want_warnings: vec![
                    "{base}/root/block/SKILL.md: line 3: \"description\" uses a block scalar (multi-line value), which is not supported; key dropped",
                ],
            },
            ScanCase {
                name: "path separator in name refused",
                files: vec![(
                    "root/named/SKILL.md",
                    "---\nname: ../escape\ndescription: x\n---\n".to_string(),
                )],
                dirs: vec![],
                roots: vec!["root"],
                seam: Seam::None,
                want_skills: vec![],
                want_skipped: vec![
                    "{base}/root/named: invalid skill name \"../escape\" (no path separators or control characters)",
                ],
                want_warnings: vec![],
            },
            ScanCase {
                name: "dotdot name refused",
                files: vec![(
                    "root/dotdot/SKILL.md",
                    "---\nname: ..\ndescription: x\n---\n".to_string(),
                )],
                dirs: vec![],
                roots: vec!["root"],
                seam: Seam::None,
                want_skills: vec![],
                want_skipped: vec![
                    "{base}/root/dotdot: skill directory name \"..\" cannot be used as a skill name",
                ],
                want_warnings: vec![],
            },
            ScanCase {
                name: "missing root is a warning",
                files: vec![],
                dirs: vec![],
                roots: vec!["missing"],
                seam: Seam::None,
                want_skills: vec![],
                want_skipped: vec![],
                want_warnings: vec!["{base}/missing: root does not exist (yet); skipped"],
            },
            ScanCase {
                name: "missing root plus good root",
                files: vec![("root/good/SKILL.md", valid("good", "Good"))],
                dirs: vec![],
                roots: vec!["missing", "root"],
                seam: Seam::None,
                want_skills: vec!["good: Good []"],
                want_skipped: vec![],
                want_warnings: vec!["{base}/missing: root does not exist (yet); skipped"],
            },
            ScanCase {
                name: "root that is a file",
                files: vec![("plain.txt", "x".to_string())],
                dirs: vec![],
                roots: vec!["plain.txt"],
                seam: Seam::None,
                want_skills: vec![],
                want_skipped: vec![],
                want_warnings: vec!["{base}/plain.txt: root is not a directory; skipped"],
            },
            ScanCase {
                name: "empty root path",
                files: vec![],
                dirs: vec![],
                roots: vec![""],
                seam: Seam::None,
                want_skills: vec![],
                want_skipped: vec![],
                want_warnings: vec!["\"\": root path is empty; skipped"],
            },
            ScanCase {
                name: "repeated root scanned once",
                files: vec![("r/dup/SKILL.md", valid("dup", "Only"))],
                dirs: vec![],
                roots: vec!["r", "r"],
                seam: Seam::None,
                want_skills: vec!["dup: Only []"],
                want_skipped: vec![],
                want_warnings: vec![
                    "{base}/r/dup: same directory as {base}/r/dup (symlink loop or repeated root); skipped",
                ],
            },
            ScanCase {
                name: "unreadable SKILL.md",
                files: vec![("root/unreadable/SKILL.md", valid("unreadable", "U"))],
                dirs: vec![],
                roots: vec!["root"],
                seam: Seam::OpenSkill,
                want_skills: vec![],
                want_skipped: vec![],
                want_warnings: vec![
                    "{base}/root/unreadable/SKILL.md: SKILL.md is unreadable: open {base}/root/unreadable/SKILL.md: permission denied",
                ],
            },
            ScanCase {
                name: "unusable root",
                files: vec![],
                dirs: vec!["root"],
                roots: vec!["root"],
                seam: Seam::StatRoot,
                want_skills: vec![],
                want_skipped: vec![],
                want_warnings: vec!["{base}/root: root is not usable: permission denied; skipped"],
            },
            ScanCase {
                name: "unlistable root",
                files: vec![],
                dirs: vec!["root"],
                roots: vec!["root"],
                seam: Seam::ReadDirRoot,
                want_skills: vec![],
                want_skipped: vec![],
                want_warnings: vec!["{base}/root: cannot list root: permission denied; skipped"],
            },
            ScanCase {
                name: "uninspectable entry",
                files: vec![
                    ("root/weird/SKILL.md", valid("weird", "W")),
                    ("root/ok/SKILL.md", valid("ok", "O")),
                ],
                dirs: vec![],
                roots: vec!["root"],
                seam: Seam::StatWeird,
                want_skills: vec!["ok: O []"],
                want_skipped: vec![],
                want_warnings: vec![
                    "{base}/root/weird: cannot inspect entry: permission denied; skipped",
                ],
            },
            ScanCase {
                name: "uninspectable SKILL.md",
                files: vec![("root/blocked/SKILL.md", valid("blocked", "B"))],
                dirs: vec![],
                roots: vec!["root"],
                seam: Seam::StatSkill,
                want_skills: vec![],
                want_skipped: vec![],
                want_warnings: vec![
                    "{base}/root/blocked/SKILL.md: cannot inspect SKILL.md: permission denied",
                ],
            },
            ScanCase {
                name: "unlistable skill directory keeps the skill",
                files: vec![("root/nofiles/SKILL.md", valid("nofiles", "N"))],
                dirs: vec![],
                roots: vec!["root"],
                seam: Seam::ReadDirNoFiles,
                want_skills: vec!["nofiles: N []"],
                want_skipped: vec![],
                want_warnings: vec![
                    "{base}/root/nofiles: cannot list files: permission denied",
                ],
            },
            ScanCase {
                name: "no roots",
                files: vec![],
                dirs: vec![],
                roots: vec![],
                seam: Seam::None,
                want_skills: vec![],
                want_skipped: vec![],
                want_warnings: vec![],
            },
        ]
    }

    #[test]
    fn scan_cases() {
        for case in cases() {
            let tree = TempTree::new("case");
            for (path, content) in &case.files {
                tree.write(path, content);
            }
            for dir in &case.dirs {
                tree.mkdir(dir);
            }
            let roots: Vec<PathBuf> = case
                .roots
                .iter()
                .map(|name| {
                    if name.is_empty() {
                        PathBuf::new()
                    } else {
                        tree.path(name)
                    }
                })
                .collect();
            let fs = FakeFs::with_seam(case.seam, &tree.root);
            let set = scan_with(&fs, &roots).expect("scan must not fail");

            let skills: Vec<String> = set.skills.iter().map(render_skill).collect();
            assert_eq!(skills, case.want_skills, "skills, case: {}", case.name);
            let skipped: Vec<String> = case
                .want_skipped
                .iter()
                .map(|item| expand(item, &tree.root))
                .collect();
            let warnings: Vec<String> = case
                .want_warnings
                .iter()
                .map(|item| expand(item, &tree.root))
                .collect();
            assert_diagnostics(&set, &skipped, &warnings, case.name);
            assert_invariants(&set);
        }
    }

    #[test]
    fn load_fills_body_without_mutating_the_receiver() {
        let tree = TempTree::new("load");
        tree.write(
            "r/alpha/SKILL.md",
            "---\r\nname: alpha\r\ndescription: Alpha\r\n---\r\nline one\r\nline two\r\n",
        );
        let set = scan(&[tree.path("r")]).expect("scan");
        let skill = set.skills.first().expect("one skill");
        assert!(skill.body.is_empty());
        let loaded = skill.load().expect("load");
        assert_eq!(loaded.body, "line one\nline two\n");
        assert!(skill.body.is_empty(), "receiver mutated by load");
        assert_eq!(skill, set.skills.first().expect("still one skill"));
    }

    #[test]
    fn load_reports_missing_file_and_missing_path() {
        let tree = TempTree::new("loaderr");
        tree.write("r/alpha/SKILL.md", &valid("alpha", "Alpha"));
        let set = scan(&[tree.path("r")]).expect("scan");
        let mut skill = set.skills.first().expect("one skill").clone();
        skill.path = display(&tree.path("r/alpha/gone.md"));
        let err = skill.load().expect_err("missing file must fail");
        assert_eq!(err.kind(), ErrorKind::Io);
        assert!(err.message().contains("skill: load "));

        let no_path = Skill {
            path: "   ".to_string(),
            ..skill
        };
        let err = no_path.load().expect_err("no path must fail");
        assert_eq!(err.kind(), ErrorKind::NoPath);
        assert_eq!(err.message(), "skill: skill has no path");
        assert_eq!(
            no_path.load().expect_err("no path").message(),
            "skill: skill has no path"
        );
    }

    #[test]
    fn load_reports_unparseable_frontmatter() {
        let tree = TempTree::new("loadbad");
        tree.write(
            "r/bad/SKILL.md",
            "---\nname: bad\ndescription: D\nno closing fence\n",
        );
        let skill = Skill {
            name: "bad".to_string(),
            description: "D".to_string(),
            dir: display(&tree.path("r/bad")),
            path: display(&tree.path("r/bad/SKILL.md")),
            body: String::new(),
            files: vec![],
        };
        let err = skill.load().expect_err("unclosed fence must fail");
        assert_eq!(err.kind(), ErrorKind::InvalidData);
        assert!(err.message().contains("frontmatter is not closed"));
    }

    #[test]
    fn scan_is_deterministic() {
        let tree = TempTree::new("determinism");
        tree.write("r/e/SKILL.md", &valid("e", "E"));
        tree.write("r/a/SKILL.md", &valid("a", "A"));
        tree.write("r/c/SKILL.md", &valid("c", "C"));
        tree.write("r/b/SKILL.md", &valid("b", "B"));
        tree.write("r/d/SKILL.md", &valid("d", "D"));
        let roots = vec![tree.path("r")];
        let first = scan(&roots).expect("scan");
        for _ in 0..4 {
            assert_eq!(scan(&roots).expect("scan"), first);
        }
    }

    #[test]
    fn scan_truncates_enormous_file() {
        let tree = TempTree::new("huge");
        let mut body = valid("huge", "Huge");
        body.push_str(&"x".repeat((1 << 20) + 100));
        tree.write("r/huge/SKILL.md", &body);
        let set = scan(&[tree.path("r")]).expect("scan");
        assert_eq!(set.skills.len(), 1);
        assert_eq!(
            set.warnings,
            vec![format!(
                "{}: larger than 1048576 bytes; body truncated",
                display(&tree.path("r/huge/SKILL.md"))
            )]
        );
    }

    #[cfg(unix)]
    fn symlink_dir(target: &Path, link: &Path) -> io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    #[cfg(windows)]
    fn symlink_dir(target: &Path, link: &Path) -> io::Result<()> {
        std::os::windows::fs::symlink_dir(target, link)
    }

    #[test]
    fn symlinked_directories_are_deduped_and_not_listed_as_files() {
        let tree = TempTree::new("symlink");
        tree.mkdir("r");
        tree.write("r/real/SKILL.md", &valid("real", "Real"));
        tree.write("r/real/extra.md", "extra");
        if let Err(err) = symlink_dir(&tree.path("r/real"), &tree.path("r/alias")) {
            eprintln!("symlinks unavailable, test skipped: {err}");
            return;
        }
        if let Err(err) = symlink_dir(&tree.path("r/real"), &tree.path("r/loop")) {
            eprintln!("symlinks unavailable, test skipped: {err}");
            return;
        }
        let set = scan(&[tree.path("r")]).expect("scan");
        assert_eq!(set.skills.len(), 1, "skills: {:?}", set.skills);
        assert_eq!(set.skills[0].files, vec!["extra.md".to_string()]);
        assert_eq!(
            set.warnings
                .iter()
                .filter(|warning| warning.contains("same directory as"))
                .count(),
            2
        );
        assert_invariants(&set);
    }

    #[test]
    fn list_files_skips_symlinked_and_real_directories() {
        let dir = "/virtual/skill";
        let mut fs = FakeFs::default();
        fs.read_dir_overrides.push((
            dir.to_string(),
            vec![
                Entry {
                    name: SKILL_FILE_NAME.to_string(),
                    is_dir: false,
                    is_symlink: false,
                },
                Entry {
                    name: "notes.txt".to_string(),
                    is_dir: false,
                    is_symlink: false,
                },
                Entry {
                    name: "link-to-dir".to_string(),
                    is_dir: false,
                    is_symlink: true,
                },
                Entry {
                    name: "broken-link".to_string(),
                    is_dir: false,
                    is_symlink: true,
                },
                Entry {
                    name: "subdir".to_string(),
                    is_dir: true,
                    is_symlink: false,
                },
            ],
        ));
        fs.metadata_dirs.push("link-to-dir".to_string());
        fs.metadata_errors
            .push(("broken-link".to_string(), io::ErrorKind::NotFound));
        assert_eq!(
            list_files(&fs, dir).expect("list"),
            vec!["notes.txt".to_string()]
        );
    }

    #[test]
    fn sort_set_orders_skills_and_diagnostics() {
        let skill = |name: &str, path: &str| Skill {
            name: name.to_string(),
            description: String::new(),
            dir: name.to_string(),
            path: path.to_string(),
            body: String::new(),
            files: vec![],
        };
        let mut set = Set {
            skills: vec![skill("b", "z"), skill("a", "y"), skill("a", "x")],
            skipped: vec!["z".to_string(), "a".to_string()],
            warnings: vec!["y".to_string(), "b".to_string()],
        };
        sort_set(&mut set);
        assert_eq!(
            set.skills
                .iter()
                .map(|item| (item.name.as_str(), item.path.as_str()))
                .collect::<Vec<_>>(),
            vec![("a", "x"), ("a", "y"), ("b", "z")]
        );
        assert_eq!(set.skipped, vec!["a", "z"]);
        assert_eq!(set.warnings, vec!["b", "y"]);
    }

    #[test]
    fn validate_name_cases() {
        for name in ["alpha", "with-dash_2.ok", "Ünicode"] {
            assert!(validate_name(name).is_ok(), "should accept {name}");
        }
        for name in ["", ".", ".."] {
            assert_eq!(
                validate_name(name),
                Err(format!(
                    "skill directory name {} cannot be used as a skill name",
                    quote(name)
                ))
            );
        }
        for name in ["../escape", "a/b", "a\\b", "bad\nname", "del\u{7f}ete"] {
            assert_eq!(
                validate_name(name),
                Err(format!(
                    "invalid skill name {} (no path separators or control characters)",
                    quote(name)
                ))
            );
        }
    }

    #[test]
    fn real_key_falls_back_to_a_clean_path() {
        let mut fs = FakeFs::default();
        fs.canonicalize_errors.push("missing".to_string());
        assert_eq!(real_key(&fs, Path::new("a/../missing")), "missing");
    }

    #[test]
    fn public_alias_matches_scan() {
        let tree = TempTree::new("alias");
        tree.write("r/a/SKILL.md", &valid("a", "A"));
        let roots = vec![tree.path("r")];
        assert_eq!(Scan(&roots).expect("Scan"), scan(&roots).expect("scan"));
    }
}
