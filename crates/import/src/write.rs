//! Atomic, additive commit of a planned import.
//!
//! The log is written to `<target>.tmp`, validated with the same `parse_log`
//! a reader uses, synced, then linked into its final name. A reader never sees
//! a half-written session and a failure leaves no temp file behind. An existing
//! session is never replaced: the log is the only truth of a session.

use std::ffi::OsString;
use std::fs::File;
use std::io::{BufWriter, Write as _};
use std::path::{Path, PathBuf};

use forge_core::event::parse_log;
use forge_core::{Error, ErrorKind, Result};

use crate::error::{err, ERR_EXISTS, ERR_PLAN_STALE};
use crate::mapping::events_for;
use crate::Source;

use super::Plan;

/// Map `src` again with the plan's session id, validate the result, and commit
/// it under `<data_dir>/sessions/<session_id>.jsonl`. Returns the session id.
pub fn write(data_dir: &Path, src: &Source, plan: &Plan) -> Result<String> {
    if data_dir.as_os_str().is_empty() {
        return Err(Error::new(ErrorKind::Invalid, "no data directory"));
    }
    let (events, dropped) = events_for(src, &plan.session_id)?;
    check_plan(plan, events.len(), &dropped)?;

    let sessions_dir = data_dir.join("sessions");
    std::fs::create_dir_all(&sessions_dir).map_err(|e| {
        err(
            ErrorKind::Io,
            "cannot create sessions directory",
            format!("{}: {e}", sessions_dir.display()),
        )
    })?;
    let final_path = sessions_dir.join(format!("{}.jsonl", plan.session_id));
    match std::fs::symlink_metadata(&final_path) {
        Ok(_) => return Err(exists_error(&final_path)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(err(
                ErrorKind::Io,
                "cannot inspect target session",
                format!("{}: {e}", final_path.display()),
            ))
        }
    }

    let mut tmp_os: OsString = final_path.as_os_str().to_os_string();
    tmp_os.push(".tmp");
    let temp = TempFile(PathBuf::from(tmp_os));
    {
        let file = File::create(&temp.0).map_err(|e| {
            err(
                ErrorKind::Io,
                "cannot create temporary log",
                format!("{}: {e}", temp.0.display()),
            )
        })?;
        let mut writer = BufWriter::new(file);
        for event in &events {
            let line = event.canonical()?;
            writer.write_all(line.as_bytes())?;
            writer.write_all(b"\n")?;
        }
        writer.flush()?;
        let file = writer.into_inner().map_err(|e| e.into_error())?;
        file.sync_all()?;
    }

    // Read back with the reader the rest of the workspace uses, before the
    // file acquires its final name. A bad import never becomes a session.
    let text = std::fs::read_to_string(&temp.0).map_err(|e| {
        err(
            ErrorKind::Io,
            "cannot read back temporary log",
            format!("{}: {e}", temp.0.display()),
        )
    })?;
    let reparsed = parse_log(&text).map_err(|e| {
        err(
            ErrorKind::Invalid,
            "imported log failed validation",
            format!("{e}"),
        )
    })?;
    if reparsed.len() != events.len() {
        return Err(err(
            ErrorKind::Invalid,
            "imported log failed validation",
            format!(
                "expected {} events, reader found {}",
                events.len(),
                reparsed.len()
            ),
        ));
    }
    if reparsed
        .iter()
        .any(|event| event.session != plan.session_id)
    {
        return Err(err(
            ErrorKind::Invalid,
            "imported log failed validation",
            "an event names a different session",
        ));
    }

    commit(&temp.0, &final_path)?;
    Ok(plan.session_id.clone())
}

/// Contract alias for [`write`].
#[allow(non_snake_case)]
pub fn Write(data_dir: &Path, src: &Source, plan: &Plan) -> Result<String> {
    write(data_dir, src, plan)
}

fn check_plan(plan: &Plan, events: usize, dropped: &[String]) -> Result<()> {
    if plan.session_id.is_empty() {
        return Err(err(
            ErrorKind::Conflict,
            ERR_PLAN_STALE,
            "plan has no session id",
        ));
    }
    if plan.events != events {
        return Err(err(
            ErrorKind::Conflict,
            ERR_PLAN_STALE,
            format!(
                "plan expects {} events, source now maps to {events}",
                plan.events
            ),
        ));
    }
    if plan.dropped.len() != dropped.len() {
        return Err(err(
            ErrorKind::Conflict,
            ERR_PLAN_STALE,
            format!(
                "plan lists {} dropped records, source now yields {}",
                plan.dropped.len(),
                dropped.len()
            ),
        ));
    }
    for (index, (planned, actual)) in plan.dropped.iter().zip(dropped).enumerate() {
        if planned != actual {
            return Err(err(
                ErrorKind::Conflict,
                ERR_PLAN_STALE,
                format!("dropped record {index} changed: plan {planned:?}, source {actual:?}"),
            ));
        }
    }
    Ok(())
}

fn exists_error(path: &Path) -> Error {
    err(ErrorKind::Conflict, ERR_EXISTS, path.display().to_string())
}

/// Link-then-unlink is the portable "fail if the target exists" commit. The
/// rename fallback is guarded by the same existence check so it cannot
/// overwrite a session either.
fn commit(temp: &Path, final_path: &Path) -> Result<()> {
    match std::fs::hard_link(temp, final_path) {
        Ok(()) => return Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(exists_error(final_path))
        }
        Err(_) => {}
    }
    if std::fs::symlink_metadata(final_path).is_ok() {
        return Err(exists_error(final_path));
    }
    std::fs::rename(temp, final_path).map_err(|e| {
        err(
            ErrorKind::Io,
            "cannot commit imported session",
            format!("{}: {e}", final_path.display()),
        )
    })
}

/// Removes the temp file on every exit path, including a failed write. After
/// a successful hard link the final name still holds the bytes, so dropping
/// this guard cannot lose the import.
struct TempFile(PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::is_exists;

    #[test]
    fn an_existing_target_is_a_conflict() {
        // The predicate is the public contract; the message keeps the tag first.
        let error = exists_error(Path::new("sessions/example.jsonl"));
        assert!(is_exists(&error));
        assert!(error.message().starts_with(ERR_EXISTS));
    }
}
