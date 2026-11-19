//! `forge market`: search an index, install a bundle, and take it back out.
//!
//! A bundle is a gzipped tarball whose digest is published in the index. The
//! crate does the verifying (`crates/market`); this command is the part that
//! knows where the index lives, where bundles go, and how to say what happened.
//!
//! Nothing here is fetched for `list`, `verify`, `remove` or `pack`: those read
//! or write local files. Only `search` and `install` need an index, and only
//! `install` downloads a tarball. `pack` is the producer step: it turns a plugin
//! directory into the tarball an index describes, using the market's own codec.

use std::io::Write;
use std::path::{Path, PathBuf};

use forge_market::{Bundle, Removed, Store};

const EXIT_OK: i32 = 0;
const EXIT_FAILURE: i32 = 1;
const EXIT_USAGE: i32 = 2;

pub const USAGE: &str = "usage: forge market <command> [flags]

Install plugin bundles from an index into <dataDir>/market. A bundle is a gzipped
tarball; the index publishes its SHA-256 and the install refuses any tarball that
does not match, so a truncated or edited download never reaches `bundles/`.

commands:
  list                 show the installed bundles and their versions
  search <text>        list index entries whose name or description matches
  install <name>       install a bundle (accepts name or name@version)
  remove <name>        uninstall a bundle, refusing if a file was edited
  verify [name]        report files whose digest no longer matches
  pack <dir>           pack a plugin directory into a bundle tarball

flags:
  --json               machine-readable output
  --data-dir <dir>     data directory (default: the forge home's configured one)
  --root <dir>         market root (default: <dataDir>/market)
  --index <url>        index URL; required by `search` and `install`
  --force              uninstall before installing (refuses edits either way)
  --out <dir>          where `pack` writes the tarball (default: .)
  --base-url <url>     `pack`: URL prefix for the tarball; writes/merges index.json
  --os <os>            `pack`: target os for a native build (windows/linux/darwin)
  --arch <arch>        `pack`: target arch for a native build (amd64/arm64)
  -h, --help           this message

A requirement is an exact version or `*`. Ranges are refused on purpose: `*`
resolves only when the index publishes exactly one version of a name, because
picking the highest of several would be this tool making a choice the operator
did not.

exit codes: 0 ok, 1 failure, 2 usage
";

const VALUE_FLAGS: &[&str] = &["data-dir", "root", "index", "out", "base-url", "os", "arch"];

#[derive(Debug, Default)]
struct Options {
    json: bool,
    data_dir: String,
    root: String,
    index: String,
    force: bool,
    out: String,
    base_url: String,
    os: String,
    arch: String,
}

pub fn run(args: &[String], stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    if crate::wants_help(args) {
        let _ = stdout.write_all(USAGE.as_bytes());
        return EXIT_OK;
    }
    let reordered = crate::reorder_flags(args, VALUE_FLAGS);
    let Some(command) = crate::command_after_flags(&reordered, VALUE_FLAGS) else {
        let _ = stderr.write_all(USAGE.as_bytes());
        return EXIT_USAGE;
    };
    let opt = match parse(&reordered, stderr) {
        Ok(opt) => opt,
        Err(code) => return code,
    };
    let store = match store_of(&opt) {
        Ok(store) => store,
        Err(err) => return fail(stderr, "forge market", &err),
    };

    let positional = positional(&reordered, &command);
    match command.as_str() {
        "list" => list(&store, &opt, stdout, stderr),
        "search" => search(&opt, &positional, stdout, stderr),
        "install" => install(&store, &opt, &positional, stdout, stderr),
        "remove" => remove(&store, &opt, &positional, stdout, stderr),
        "verify" => verify(&store, &opt, &positional, stdout, stderr),
        "pack" => pack(&opt, &positional, stdout, stderr),
        other => {
            let _ = writeln!(stderr, "forge market: unknown command {other:?}\n\n{USAGE}");
            EXIT_USAGE
        }
    }
}

fn parse(args: &[String], stderr: &mut dyn Write) -> std::result::Result<Options, i32> {
    let mut opt = Options::default();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        // Positionals belong to the dispatch arm (`forge market install <name>`).
        if !arg.starts_with('-') {
            i += 1;
            continue;
        }
        let name = arg.trim_start_matches('-');
        let (name, inline) = match name.split_once('=') {
            Some((name, value)) => (name.to_string(), Some(value.to_string())),
            None => (name.to_string(), None),
        };
        let mut take = |index: &mut usize| -> std::result::Result<String, i32> {
            match inline.clone() {
                Some(value) => Ok(value),
                None => {
                    *index += 1;
                    match args.get(*index) {
                        Some(value) => Ok(value.clone()),
                        None => {
                            let _ = writeln!(stderr, "forge market: --{name} needs a value");
                            Err(EXIT_USAGE)
                        }
                    }
                }
            }
        };
        match name.as_str() {
            "json" if inline.is_none() => opt.json = true,
            "force" if inline.is_none() => opt.force = true,
            "data-dir" => opt.data_dir = take(&mut i)?,
            "root" => opt.root = take(&mut i)?,
            "index" => opt.index = take(&mut i)?,
            "out" => opt.out = take(&mut i)?,
            "base-url" => opt.base_url = take(&mut i)?,
            "os" => opt.os = take(&mut i)?,
            "arch" => opt.arch = take(&mut i)?,
            "" => {}
            other => {
                let _ = writeln!(stderr, "forge market: unknown flag --{other}");
                return Err(EXIT_USAGE);
            }
        }
        i += 1;
    }
    Ok(opt)
}

fn store_of(opt: &Options) -> forge_core::Result<Store> {
    if !opt.root.trim().is_empty() {
        return Ok(Store::new(PathBuf::from(opt.root.trim())));
    }
    let data_dir = if opt.data_dir.trim().is_empty() {
        crate::default_data_dir()?
    } else {
        PathBuf::from(opt.data_dir.trim())
    };
    Ok(Store::new(data_dir.join("market")))
}

fn positional<'a>(args: &'a [String], command: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut i = 0;
    let mut seen_command = false;
    while i < args.len() {
        let arg = args[i].as_str();
        if !seen_command && arg == command {
            seen_command = true;
            i += 1;
            continue;
        }
        if arg == "--" {
            i += 1;
            while i < args.len() {
                out.push(args[i].as_str());
                i += 1;
            }
            break;
        }
        if arg.starts_with("--") {
            let name = arg.trim_start_matches('-');
            if !name.contains('=') && VALUE_FLAGS.contains(&name) {
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        if arg.starts_with('-') {
            i += 1;
            continue;
        }
        out.push(arg);
        i += 1;
    }
    out
}

fn fail(stderr: &mut dyn Write, command: &str, err: &forge_core::Error) -> i32 {
    let _ = writeln!(stderr, "{command}: {}", err.message());
    EXIT_FAILURE
}

fn list(store: &Store, opt: &Options, stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    let records = match forge_market::list(store) {
        Ok(records) => records,
        Err(err) => return fail(stderr, "forge market", &err),
    };
    if opt.json {
        let rows: Vec<serde_json::Value> = records
            .iter()
            .map(|record| {
                serde_json::json!({
                    "name": record.name,
                    "version": record.version,
                    "sha256": record.sha256,
                    "installedAt": record.installed_at,
                    "files": record.files.len(),
                    "path": store.bundle_dir(&record.name).display().to_string(),
                })
            })
            .collect();
        return crate::write_json(
            stdout,
            stderr,
            "forge market",
            &serde_json::json!({
                "root": store.root().display().to_string(),
                "bundles": rows,
            }),
        );
    }
    if records.is_empty() {
        let _ = writeln!(
            stdout,
            "no bundles installed under {}",
            store.root().display()
        );
        return EXIT_OK;
    }
    let _ = writeln!(
        stdout,
        "{:<24} {:<12} {:>5}  installed",
        "name", "version", "files"
    );
    for record in &records {
        let _ = writeln!(
            stdout,
            "{:<24} {:<12} {:>5}  {}",
            record.name,
            record.version,
            record.files.len(),
            record.installed_at
        );
    }
    EXIT_OK
}

fn search(
    opt: &Options,
    positional: &[&str],
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    let Some(needle) = positional.first() else {
        let _ = writeln!(stderr, "forge market: search needs a word to look for");
        return EXIT_USAGE;
    };
    let index = match fetch_index(opt, stdout, stderr) {
        Ok(index) => index,
        Err(code) => return code,
    };
    let needle = needle.to_lowercase();
    let hits: Vec<&Bundle> = index
        .bundles
        .iter()
        .filter(|bundle| {
            bundle.name.to_lowercase().contains(&needle)
                || bundle.description.to_lowercase().contains(&needle)
        })
        .collect();
    // The host platform is reported alongside the rows, because "is there a
    // build for me" is the first question a native bundle raises and the answer
    // is not visible in a name and a version.
    let here = forge_market::host_platform();
    if opt.json {
        let rows: Vec<serde_json::Value> = hits
            .iter()
            .map(|bundle| {
                serde_json::json!({
                    "name": bundle.name,
                    "version": bundle.version,
                    "description": bundle.description,
                    "tarball": bundle.tarball,
                    "size": bundle.size,
                    "platform": bundle.platform_label(),
                    "fitsThisHost": bundle.fits(&here),
                })
            })
            .collect();
        return crate::write_json(
            stdout,
            stderr,
            "forge market",
            &serde_json::json!({
                "query": needle,
                "host": here.label(),
                "bundles": rows,
            }),
        );
    }
    if hits.is_empty() {
        let _ = writeln!(stdout, "nothing in the index matches {needle:?}");
        return EXIT_OK;
    }
    let _ = writeln!(
        stdout,
        "{:<24} {:<12} {:<16}  description",
        "name", "version", "platform"
    );
    for bundle in &hits {
        let _ = writeln!(
            stdout,
            "{:<24} {:<12} {:<16}  {}",
            bundle.name,
            bundle.version,
            bundle.platform_label(),
            bundle.description
        );
    }
    // A hit that cannot be installed here is stated, not left to be discovered
    // by a failed install.
    let unusable = hits.iter().filter(|bundle| !bundle.fits(&here)).count();
    if unusable > 0 {
        let _ = writeln!(
            stderr,
            "forge market: {unusable} of {} row(s) have no build for {}; install would refuse them",
            hits.len(),
            here.label()
        );
    }
    EXIT_OK
}

fn install(
    store: &Store,
    opt: &Options,
    positional: &[&str],
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    let Some(spec) = positional.first() else {
        let _ = writeln!(stderr, "forge market: install needs a bundle name");
        return EXIT_USAGE;
    };
    let (name, requirement) = match spec.split_once('@') {
        Some((name, version)) => (name, version),
        None => (*spec, "*"),
    };
    if name.trim().is_empty() {
        let _ = writeln!(stderr, "forge market: install needs a bundle name");
        return EXIT_USAGE;
    }
    let index = match fetch_index(opt, stdout, stderr) {
        Ok(index) => index,
        Err(code) => return code,
    };
    let bundle = match index.resolve(name, requirement) {
        Ok(bundle) => bundle.clone(),
        Err(err) => return fail(stderr, "forge market", &err),
    };
    if opt.force {
        // `install` refuses an existing directory outright, and it is right to:
        // deleting a directory no record describes is the loss the crate exists
        // to prevent. So `--force` is an explicit uninstall first, not an
        // overwrite — and the uninstall keeps its own tamper check.
        match forge_market::uninstall(store, &bundle.name) {
            Ok(removed) => report_removed(stdout, &removed),
            Err(err) if err.kind() == forge_core::ErrorKind::NotFound => {}
            Err(err) => return fail(stderr, "forge market", &err),
        }
    }
    let client = match market_client() {
        Ok(client) => client,
        Err(err) => return fail(stderr, "forge market", &err),
    };
    let downloads = store.root().join("downloads");
    if let Err(err) = std::fs::create_dir_all(&downloads) {
        let _ = writeln!(
            stderr,
            "forge market: cannot create {}: {err}",
            downloads.display()
        );
        return EXIT_FAILURE;
    }
    let tarball = match forge_market::download(&client, &bundle, &downloads) {
        Ok(path) => path,
        Err(err) => return fail(stderr, "forge market", &err),
    };
    let installed = match forge_market::install(store, &bundle, &tarball) {
        Ok(installed) => installed,
        Err(err) => return fail(stderr, "forge market", &err),
    };
    // The tarball has done its job: keeping it would double the disk cost of
    // every install and give a stale copy a chance to be reinstalled later.
    let _ = std::fs::remove_file(&tarball);
    if opt.json {
        return crate::write_json(
            stdout,
            stderr,
            "forge market",
            &serde_json::json!({
                "name": installed.name,
                "version": installed.version,
                "path": installed.path.display().to_string(),
                "files": installed.files,
            }),
        );
    }
    let _ = writeln!(
        stdout,
        "{} {} installed ({} file(s))",
        installed.name,
        installed.version,
        installed.files.len()
    );
    let _ = writeln!(stdout, "  {}", installed.path.display());
    EXIT_OK
}

fn remove(
    store: &Store,
    opt: &Options,
    positional: &[&str],
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    let Some(name) = positional.first() else {
        let _ = writeln!(stderr, "forge market: remove needs a bundle name");
        return EXIT_USAGE;
    };
    let removed = match forge_market::uninstall(store, name) {
        Ok(removed) => removed,
        Err(err) => return fail(stderr, "forge market", &err),
    };
    if opt.json {
        return crate::write_json(
            stdout,
            stderr,
            "forge market",
            &serde_json::json!({
                "name": removed.name,
                "version": removed.version,
                "files": removed.files,
                "directories": removed.directories,
            }),
        );
    }
    report_removed(stdout, &removed);
    EXIT_OK
}

fn report_removed(stdout: &mut dyn Write, removed: &Removed) {
    let _ = writeln!(
        stdout,
        "{} {} removed ({} file(s))",
        removed.name,
        removed.version,
        removed.files.len()
    );
}

fn verify(
    store: &Store,
    opt: &Options,
    positional: &[&str],
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    let names: Vec<String> = match positional.first() {
        Some(name) => vec![(*name).to_string()],
        None => match forge_market::list(store) {
            Ok(records) => records.into_iter().map(|record| record.name).collect(),
            Err(err) => return fail(stderr, "forge market", &err),
        },
    };
    if names.is_empty() {
        let _ = writeln!(
            stdout,
            "no bundles installed under {}",
            store.root().display()
        );
        return EXIT_OK;
    }
    let mut drifted: Vec<(String, Vec<String>)> = Vec::new();
    for name in &names {
        match forge_market::modified_files(store, name) {
            Ok(files) if !files.is_empty() => drifted.push((name.clone(), files)),
            Ok(_) => {}
            Err(err) => return fail(stderr, "forge market", &err),
        }
    }
    if opt.json {
        let rows: Vec<serde_json::Value> = drifted
            .iter()
            .map(|(name, files)| serde_json::json!({ "name": name, "modified": files }))
            .collect();
        let _ = serde_json::to_writer(&mut *stdout, &serde_json::json!({ "drift": rows }));
        let _ = writeln!(stdout);
    } else if drifted.is_empty() {
        let _ = writeln!(stdout, "every installed file still matches its digest");
    } else {
        for (name, files) in &drifted {
            let _ = writeln!(stdout, "{name}: {} file(s) changed", files.len());
            for file in files {
                let _ = writeln!(stdout, "  {file}");
            }
        }
        let _ = writeln!(
            stderr,
            "forge market: drift is not an error this command can fix; `remove` refuses too"
        );
    }
    // Drift is reported, not repaired, so a clean run is the only zero. A caller
    // that wants "did anything change" reads the output, not the exit code.
    if drifted.is_empty() {
        EXIT_OK
    } else {
        EXIT_FAILURE
    }
}

/// Pack a plugin directory into a bundle tarball, and optionally an index entry.
///
/// This is the producer step that closes the loop `new -> pack -> install`: the
/// tarball is written with the market's own codec, so the digest printed here is
/// the one `install` will verify, by construction rather than by a second
/// implementation agreeing with the first.
fn pack(opt: &Options, positional: &[&str], stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    let Some(dir) = positional.first() else {
        let _ = writeln!(stderr, "forge market: pack needs a plugin directory");
        return EXIT_USAGE;
    };
    let dir = PathBuf::from(dir);
    let manifest = match forge_plugin::manifest::read(&dir) {
        Ok(manifest) => manifest,
        Err(err) => {
            let _ = writeln!(stderr, "forge market: {}", err.message());
            return EXIT_FAILURE;
        }
    };

    // A native build declares both os and arch; a source plugin declares neither.
    let os = opt.os.trim();
    let arch = opt.arch.trim();
    let platform = match (os.is_empty(), arch.is_empty()) {
        (true, true) => None,
        (false, false) => {
            if !forge_market::KNOWN_OS.contains(&os) {
                let _ = writeln!(
                    stderr,
                    "forge market: --os {os:?} is not one of {}",
                    forge_market::KNOWN_OS.join(", ")
                );
                return EXIT_USAGE;
            }
            if !forge_market::KNOWN_ARCH.contains(&arch) {
                let _ = writeln!(
                    stderr,
                    "forge market: --arch {arch:?} is not one of {}",
                    forge_market::KNOWN_ARCH.join(", ")
                );
                return EXIT_USAGE;
            }
            Some((os.to_string(), arch.to_string()))
        }
        _ => {
            let _ = writeln!(
                stderr,
                "forge market: --os and --arch must be given together, or not at all"
            );
            return EXIT_USAGE;
        }
    };

    let packed = match forge_market::pack_dir(&dir) {
        Ok(packed) => packed,
        Err(err) => return fail(stderr, "forge market", &err),
    };

    let out = if opt.out.trim().is_empty() {
        PathBuf::from(".")
    } else {
        PathBuf::from(opt.out.trim())
    };
    if let Err(err) = std::fs::create_dir_all(&out) {
        let _ = writeln!(
            stderr,
            "forge market: cannot create {}: {err}",
            out.display()
        );
        return EXIT_FAILURE;
    }
    let suffix = match &platform {
        Some((os, arch)) => format!("-{os}-{arch}"),
        None => String::new(),
    };
    let filename = format!("{}-{}{}.tar.gz", manifest.name, manifest.version, suffix);
    let tarball_path = out.join(&filename);
    if let Err(err) = std::fs::write(&tarball_path, &packed.tarball) {
        let _ = writeln!(
            stderr,
            "forge market: cannot write {}: {err}",
            tarball_path.display()
        );
        return EXIT_FAILURE;
    }

    // With a base URL the tarball has a home, so an index entry can be written.
    // Without one the tarball is still produced, but its URL is the publisher's
    // to decide; the digest and size are printed so the entry can be completed.
    let base = opt.base_url.trim();
    let index_path = if base.is_empty() {
        None
    } else {
        let bundle = Bundle {
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            description: manifest.description.clone(),
            author: None,
            keywords: None,
            homepage: None,
            tarball: format!("{}/{}", base.trim_end_matches('/'), filename),
            sha256: packed.sha256.clone(),
            size: packed.size,
            engines: None,
            os: platform.as_ref().map(|(os, _)| os.clone()),
            arch: platform.as_ref().map(|(_, arch)| arch.clone()),
        };
        match merge_index_entry(&out, bundle) {
            Ok(path) => Some(path),
            Err(err) => return fail(stderr, "forge market", &err),
        }
    };

    if opt.json {
        return crate::write_json(
            stdout,
            stderr,
            "forge market",
            &serde_json::json!({
                "name": manifest.name,
                "version": manifest.version,
                "tarball": tarball_path.display().to_string(),
                "sha256": packed.sha256,
                "size": packed.size,
                "files": packed.files,
                "platform": platform.map(|(os, arch)| format!("{os}/{arch}")),
                "index": index_path.map(|p| p.display().to_string()),
            }),
        );
    }
    let _ = writeln!(
        stdout,
        "packed {} {} ({} file(s))",
        manifest.name,
        manifest.version,
        packed.files.len()
    );
    let _ = writeln!(stdout, "  {}", tarball_path.display());
    let _ = writeln!(stdout, "  sha256 {}", packed.sha256);
    let _ = writeln!(stdout, "  size   {}", packed.size);
    match index_path {
        Some(path) => {
            let _ = writeln!(stdout, "  index  {}", path.display());
        }
        None => {
            let _ = writeln!(
                stderr,
                "forge market: no --base-url, so no index was written; host the tarball and add an entry with this sha256 and size"
            );
        }
    }
    EXIT_OK
}

/// Add or replace one bundle in `<out>/index.json`, keeping the other entries.
///
/// The merged document is re-validated before it is written, so a pack cannot
/// leave behind an index the consumer would refuse. The key is name + version +
/// os + arch, the same one the index enforces.
fn merge_index_entry(out: &Path, bundle: Bundle) -> forge_core::Result<PathBuf> {
    let index_path = out.join("index.json");
    let mut bundles: Vec<Bundle> = if index_path.exists() {
        let bytes = std::fs::read(&index_path).map_err(|err| {
            forge_core::Error::new(
                forge_core::ErrorKind::Io,
                format!("cannot read {}: {err}", index_path.display()),
            )
        })?;
        forge_market::Index::parse(&bytes)?.bundles
    } else {
        Vec::new()
    };
    let key = |b: &Bundle| {
        (
            b.name.clone(),
            b.version.clone(),
            b.os.clone(),
            b.arch.clone(),
        )
    };
    if let Some(position) = bundles.iter().position(|b| key(b) == key(&bundle)) {
        bundles[position] = bundle;
    } else {
        bundles.push(bundle);
    }
    let mut index = forge_market::Index { bundles };
    index.validate()?;
    let doc = serde_json::json!({
        "version": forge_market::INDEX_VERSION,
        "bundles": index.bundles,
    });
    let text = serde_json::to_string_pretty(&doc).map_err(|err| {
        forge_core::Error::new(
            forge_core::ErrorKind::Json,
            format!("cannot encode index: {err}"),
        )
    })?;
    std::fs::write(&index_path, text).map_err(|err| {
        forge_core::Error::new(
            forge_core::ErrorKind::Io,
            format!("cannot write {}: {err}", index_path.display()),
        )
    })?;
    Ok(index_path)
}

fn market_client() -> forge_core::Result<forge_net::Client> {
    // The default 2 MiB body cap is right for an index and wrong for a tarball,
    // so the download client is the one place that raises it.
    forge_net::Client::new(forge_net::ClientOptions {
        max_bytes: 64 << 20,
        user_agent: format!("forge/{} (market)", crate::VERSION),
        ..Default::default()
    })
}

fn fetch_index(
    opt: &Options,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> std::result::Result<forge_market::Index, i32> {
    let _ = stdout;
    if opt.index.trim().is_empty() {
        let _ = writeln!(
            stderr,
            "forge market: no index configured; pass --index <url> (there is no default: an index this tool invented would be an index nobody published)"
        );
        return Err(EXIT_USAGE);
    }
    let client = match market_client() {
        Ok(client) => client,
        Err(err) => {
            let _ = writeln!(stderr, "forge market: {}", err.message());
            return Err(EXIT_FAILURE);
        }
    };
    let response = match client.get(opt.index.trim()) {
        Ok(response) => response,
        Err(err) => {
            let _ = writeln!(stderr, "forge market: {}", err.message());
            return Err(EXIT_FAILURE);
        }
    };
    if !response.is_success() {
        let _ = writeln!(
            stderr,
            "forge market: the index answered {}",
            response.error_summary()
        );
        return Err(EXIT_FAILURE);
    }
    match forge_market::Index::parse(&response.body) {
        Ok(index) => Ok(index),
        Err(err) => {
            let _ = writeln!(stderr, "forge market: {}", err.message());
            Err(EXIT_FAILURE)
        }
    }
}

/// The index file as the tests and an operator's `forge market search` see it.
#[allow(dead_code)]
fn read_index_file(path: &Path) -> forge_core::Result<forge_market::Index> {
    let bytes = std::fs::read(path)?;
    forge_market::Index::parse(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_market(args: &[&str]) -> (i32, String, String) {
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
        let mut stdout: Vec<u8> = Vec::new();
        let mut stderr: Vec<u8> = Vec::new();
        let code = run(&args, &mut stdout, &mut stderr);
        (
            code,
            String::from_utf8_lossy(&stdout).to_string(),
            String::from_utf8_lossy(&stderr).to_string(),
        )
    }

    fn scratch(tag: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "forge-market-cli-{}-{tag}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create scratch");
        path
    }

    #[test]
    fn the_usage_names_every_command() {
        for command in ["list", "search", "install", "remove", "verify", "pack"] {
            assert!(
                USAGE.contains(&format!("  {command}")),
                "usage lacks {command}"
            );
        }
    }

    /// A minimal plugin directory: a manifest and one entry file.
    fn plugin_dir(root: &Path, name: &str) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).expect("create plugin dir");
        std::fs::write(
            dir.join("plugin.json"),
            format!(
                "{{\"name\":\"{name}\",\"version\":\"1.0.0\",\"description\":\"a test plugin\",\"runtime\":\"node\",\"entry\":\"index.js\"}}"
            ),
        )
        .expect("write manifest");
        std::fs::write(dir.join("index.js"), "// a plugin\n").expect("write entry");
        dir
    }

    #[test]
    fn pack_produces_a_tarball_and_index_the_consumer_installs() {
        let root = scratch("pack-roundtrip");
        let dir = plugin_dir(&root, "hello");
        let out = root.join("dist");
        let (code, stdout, stderr) = run_market(&[
            "pack",
            &dir.display().to_string(),
            "--out",
            &out.display().to_string(),
            "--base-url",
            "https://example.invalid/plugins",
        ]);
        assert_eq!(code, 0, "{stderr}");
        assert!(stdout.contains("packed hello 1.0.0"));

        // The index the packer wrote must parse and resolve to the bundle.
        let index_bytes = std::fs::read(out.join("index.json")).expect("index written");
        let index = forge_market::Index::parse(&index_bytes).expect("index parses");
        let bundle = index.resolve("hello", "1.0.0").expect("resolve");
        assert_eq!(
            bundle.tarball,
            "https://example.invalid/plugins/hello-1.0.0.tar.gz"
        );

        // The tarball on disk must match the digest the index declares, and the
        // consumer must install it — the whole producer/consumer loop, in process.
        let tarball = std::fs::read(out.join("hello-1.0.0.tar.gz")).expect("tarball");
        assert_eq!(forge_market::sha256_hex(&tarball), bundle.sha256);
        assert_eq!(tarball.len() as u64, bundle.size);
        let store = forge_market::Store::new(root.join("market"));
        let installed = forge_market::install_bytes(&store, bundle, &tarball)
            .expect("install the packed bundle");
        assert_eq!(installed.name, "hello");
        assert!(installed.files.iter().any(|f| f == "plugin.json"));
        assert!(installed.files.iter().any(|f| f == "index.js"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn pack_without_a_base_url_writes_the_tarball_but_no_index() {
        let root = scratch("pack-nourl");
        let dir = plugin_dir(&root, "solo");
        let out = root.join("dist");
        let (code, _stdout, stderr) = run_market(&[
            "pack",
            &dir.display().to_string(),
            "--out",
            &out.display().to_string(),
        ]);
        assert_eq!(code, 0);
        assert!(out.join("solo-1.0.0.tar.gz").is_file());
        assert!(!out.join("index.json").exists());
        assert!(stderr.contains("no --base-url"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn pack_refuses_half_a_platform() {
        let root = scratch("pack-halfplatform");
        let dir = plugin_dir(&root, "half");
        let (code, _stdout, stderr) = run_market(&[
            "pack",
            &dir.display().to_string(),
            "--os",
            "linux",
            "--out",
            &root.join("dist").display().to_string(),
        ]);
        assert_eq!(code, 2);
        assert!(stderr.contains("together"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn pack_replaces_an_existing_entry_for_the_same_name_version_platform() {
        let root = scratch("pack-merge");
        let dir = plugin_dir(&root, "dup");
        let out = root.join("dist");
        let out_str = out.display().to_string();
        let dir_str = dir.display().to_string();
        let base = "https://example.invalid/p";
        let (first, _, _) = run_market(&["pack", &dir_str, "--out", &out_str, "--base-url", base]);
        assert_eq!(first, 0);
        // Change the entry bytes so the digest differs, then pack again.
        std::fs::write(dir.join("index.js"), "// changed\n").expect("edit");
        let (second, _, _) = run_market(&["pack", &dir_str, "--out", &out_str, "--base-url", base]);
        assert_eq!(second, 0);
        let index =
            forge_market::Index::parse(&std::fs::read(out.join("index.json")).expect("index"))
                .expect("parse");
        assert_eq!(
            index.bundles.len(),
            1,
            "the entry was replaced, not duplicated"
        );
        let tarball = std::fs::read(out.join("dup-1.0.0.tar.gz")).expect("tarball");
        assert_eq!(index.bundles[0].sha256, forge_market::sha256_hex(&tarball));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_unknown_command_is_a_usage_error() {
        let (code, stdout, stderr) = run_market(&["nope"]);
        assert_eq!(code, 2);
        assert!(stdout.is_empty());
        assert!(stderr.contains("unknown command \"nope\""));
    }

    #[test]
    fn listing_an_empty_root_says_so_and_creates_nothing() {
        let root = scratch("list-empty");
        let target = root.join("market");
        let (code, stdout, _stderr) =
            run_market(&["list", "--root", &target.display().to_string()]);
        assert_eq!(code, 0);
        assert!(stdout.contains("no bundles installed"));
        assert!(
            !target.exists(),
            "a listing must not create the market root"
        );
    }

    #[test]
    fn installing_without_an_index_is_a_usage_error_and_names_the_flag() {
        let root = scratch("install-no-index");
        let (code, _stdout, stderr) = run_market(&[
            "install",
            "beta",
            "--root",
            &root.join("market").display().to_string(),
        ]);
        assert_eq!(code, 2);
        assert!(stderr.contains("--index"), "{stderr}");
    }

    #[test]
    fn install_and_remove_need_a_name() {
        for command in ["install", "remove"] {
            let (code, _stdout, stderr) = run_market(&[command]);
            assert_eq!(code, 2, "{command}");
            assert!(
                stderr.contains("needs a bundle name"),
                "{command}: {stderr}"
            );
        }
    }

    #[test]
    fn a_bundle_name_splits_into_a_name_and_a_requirement() {
        let split = |spec: &str| match spec.split_once('@') {
            Some((name, version)) => (name.to_string(), version.to_string()),
            None => (spec.to_string(), "*".to_string()),
        };
        assert_eq!(split("beta"), ("beta".to_string(), "*".to_string()));
        assert_eq!(
            split("beta@1.2.3"),
            ("beta".to_string(), "1.2.3".to_string())
        );
    }

    #[test]
    fn verifying_an_empty_root_is_success() {
        let root = scratch("verify-empty");
        let target = root.join("market");
        let (code, stdout, _stderr) =
            run_market(&["verify", "--root", &target.display().to_string()]);
        assert_eq!(code, 0);
        assert!(stdout.contains("no bundles installed"));
    }

    #[test]
    fn an_unknown_flag_is_a_usage_error() {
        let (code, _stdout, stderr) = run_market(&["list", "--nope"]);
        assert_eq!(code, 2);
        assert!(stderr.contains("unknown flag --nope"), "{stderr}");
    }
}
