//! Turning attachment metadata in the log back into bytes for the model.
//!
//! The log records what a message carried, never the bytes themselves
//! (`docs/contracts/events-v1.md` §4.0.1): bodies live in `<dataDir>/blobs/`.
//! This module is the seam between those two facts — it reads the blob store for
//! the messages the projection just rebuilt and hands the agent loop the parts it
//! can actually send.
//!
//! Two rules matter here:
//!
//! 1. A body that cannot be read is skipped, not fatal. The turn still runs on
//!    the text; refusing to send anything because one screenshot was collected by
//!    `hearth doctor --prune-profiles` would be worse than sending less.
//! 2. Only images become parts. A `diff` or `file` attachment is a reference the
//!    model may fetch with a tool; inlining its bytes would be a second, silent
//!    way to spend the context window.

use std::path::{Path, PathBuf};

use hearth_llm::ContentPart;
use hearth_session::projection::{Message, MESSAGE_TOOL};

/// A hook suitable for `hearth_agent::Options::attachments`.
///
/// `data_dir` is where the server wrote the blobs, so the CLI must resolve it the
/// same way the server did: `<dataDir>/blobs`, no per-session nesting.
/// `workspace` is what a `read_image` path is relative to; passing `None` means
/// tool-returned images are not re-sent, only uploaded attachments are.
pub fn parts_hook(
    data_dir: &Path,
    workspace: Option<PathBuf>,
) -> impl Fn(&Message) -> Vec<ContentPart> + Send + Sync {
    let blobs = hearth_session::Blobs::new(data_dir.join(BLOBS_DIR));
    move |message: &Message| {
        let mut parts = parts_from(&blobs, message);
        if let Some(root) = &workspace {
            if let Some(part) = tool_image(root, message) {
                parts.push(part);
            }
        }
        parts
    }
}

/// The directory the server uses for bodies, spelled once.
pub const BLOBS_DIR: &str = "blobs";

/// The meta key `read_image` records so a later turn can find its file again.
pub const IMAGE_PATH_META: &str = "imagePath";

/// The meta key naming the image's type.
pub const MEDIA_TYPE_META: &str = "mediaType";

/// Re-read the file a tool pointed at.
///
/// A tool message carries text plus `meta.imagePath` (a workspace-relative
/// path). The bytes were never in the log, so this reads them from disk again —
/// which is the whole reason the path is recorded rather than the body.
fn tool_image(workspace: &Path, message: &Message) -> Option<ContentPart> {
    if message.kind != MESSAGE_TOOL {
        return None;
    }
    let meta = message.meta.as_ref()?;
    let relative = meta.get(IMAGE_PATH_META)?.as_str()?.trim();
    if relative.is_empty() {
        return None;
    }
    // Resolve through the shared helper so a `..` in the log cannot walk out of
    // the workspace, and re-sniff: the file may have been replaced by text
    // since the tool read it, and a wrong media type fails the request rather
    // than degrading.
    let absolute = hearth_tool::resolve(workspace, relative).ok()?;
    let data = std::fs::read(&absolute).ok()?;
    let sniffed = sniff_image(&data)?;
    Some(ContentPart::image(sniffed, data))
}

fn sniff_image(data: &[u8]) -> Option<&'static str> {
    Some(match data {
        [0x89, b'P', b'N', b'G', ..] => "image/png",
        [0xFF, 0xD8, 0xFF, ..] => "image/jpeg",
        [b'G', b'I', b'F', b'8', ..] => "image/gif",
        [b'B', b'M', ..] => "image/bmp",
        [b'I', b'I', 0x2A, 0x00, ..] | [b'M', b'M', 0x00, 0x2A, ..] => "image/tiff",
        [0x00, 0x00, 0x01, 0x00, ..] => "image/x-icon",
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => "image/webp",
        _ => return None,
    })
}

/// Does this attachment carry bytes the model can be shown?
///
/// `image/*` is the only family a chat-completions message can express. A
/// `application/pdf` attachment is a file the operator attached, not something
/// this transport can render.
fn is_image(kind: &str, media_type: &str) -> bool {
    kind == "image" || media_type.starts_with("image/")
}

fn parts_from(blobs: &hearth_session::Blobs, message: &Message) -> Vec<ContentPart> {
    message
        .attachments
        .iter()
        .filter(|attachment| is_image(&attachment.kind, &attachment.media_type))
        .filter_map(|attachment| {
            // `file` attachments name a workspace path and carry no body; only
            // an uploaded one has an id, and an id is the blob name.
            let id = attachment.id.trim();
            if id.is_empty() {
                return None;
            }
            let data = blobs.get(id).ok()?;
            let media_type = if attachment.media_type.trim().is_empty() {
                "image/png".to_string()
            } else {
                attachment.media_type.clone()
            };
            Some(ContentPart::image(media_type, data))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use hearth_session::projection::{Attachment, Message};

    fn message_with(attachments: Vec<Attachment>) -> Message {
        Message {
            kind: hearth_session::projection::MESSAGE_USER.to_string(),
            text: "look at this".to_string(),
            attachments,
            ..Message::default()
        }
    }

    fn upload(id: &str, media_type: &str) -> Attachment {
        Attachment {
            kind: "image".to_string(),
            id: id.to_string(),
            media_type: media_type.to_string(),
            name: "shot.png".to_string(),
            bytes: 4,
            ..Attachment::default()
        }
    }

    #[test]
    fn an_uploaded_image_becomes_an_image_part() {
        let dir = std::env::temp_dir().join(format!("hearth-attach-{}-hook", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let blobs = hearth_session::Blobs::new(dir.join(BLOBS_DIR));
        let id = blobs.put(&[0x89, 0x50, 0x4E, 0x47]).expect("write blob");
        let hook = parts_hook(&dir, None);
        let parts = hook(&message_with(vec![upload(&id, "image/png")]));
        assert_eq!(parts.len(), 1);
        match &parts[0] {
            ContentPart::Image { media_type, data } => {
                assert_eq!(media_type, "image/png");
                assert_eq!(data, &[0x89, 0x50, 0x4E, 0x47]);
            }
            other => panic!("expected an image part, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_message_without_attachments_contributes_nothing() {
        let dir = std::env::temp_dir().join(format!("hearth-attach-{}-plain", std::process::id()));
        let hook = parts_hook(&dir, None);
        assert!(hook(&message_with(Vec::new())).is_empty());
    }

    #[test]
    fn a_missing_body_is_skipped_rather_than_fatal() {
        // The id is well formed but no blob was ever written under it: the turn
        // must still run on the text.
        let dir = std::env::temp_dir().join(format!("hearth-attach-{}-gone", std::process::id()));
        let hook = parts_hook(&dir, None);
        let id = "0123456789abcdef0123456789abcdef";
        assert!(hook(&message_with(vec![upload(id, "image/png")])).is_empty());
    }

    #[test]
    fn a_reference_without_an_id_is_not_read() {
        let dir = std::env::temp_dir().join(format!("hearth-attach-{}-ref", std::process::id()));
        let hook = parts_hook(&dir, None);
        // `kind: file` names a workspace path; the path must never be opened here.
        let reference = Attachment {
            kind: "file".to_string(),
            path: "src/main.rs".to_string(),
            bytes: 0,
            ..Attachment::default()
        };
        assert!(hook(&message_with(vec![reference])).is_empty());
    }

    #[test]
    fn only_image_families_are_inlined() {
        assert!(is_image("image", "image/png"));
        assert!(is_image("attachment", "image/jpeg"));
        assert!(!is_image("file", "text/plain"));
        assert!(!is_image("diff", ""));
    }

    fn tool_message(meta: serde_json::Value) -> Message {
        Message {
            kind: MESSAGE_TOOL.to_string(),
            text: "read_image: shot.png (image/png, 4 bytes)".to_string(),
            call_id: "c1".to_string(),
            meta: Some(meta),
            ..Message::default()
        }
    }

    #[test]
    fn a_tool_that_returned_an_image_is_read_again_from_the_workspace() {
        let root = std::env::temp_dir().join(format!("hearth-attach-{}-tool", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let workspace = root.join("ws");
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::write(workspace.join("shot.png"), [0x89, 0x50, 0x4E, 0x47]).expect("image");
        let hook = parts_hook(&root, Some(workspace));
        let parts = hook(&tool_message(
            serde_json::json!({"imagePath": "shot.png", "mediaType": "image/png"}),
        ));
        assert_eq!(parts.len(), 1);
        match &parts[0] {
            ContentPart::Image { media_type, data } => {
                assert_eq!(media_type, "image/png");
                assert_eq!(data, &[0x89, 0x50, 0x4E, 0x47]);
            }
            other => panic!("expected an image part, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_tool_image_path_outside_the_workspace_is_not_opened() {
        let root = std::env::temp_dir().join(format!("hearth-attach-{}-esc", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let workspace = root.join("ws");
        std::fs::create_dir_all(&workspace).expect("workspace");
        // The secret lives beside the workspace, not inside it.
        std::fs::write(root.join("secret.png"), [0x89, 0x50, 0x4E, 0x47]).expect("secret");
        let hook = parts_hook(&root, Some(workspace));
        let parts = hook(&tool_message(
            serde_json::json!({"imagePath": "../secret.png"}),
        ));
        assert!(parts.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_tool_meta_naming_a_file_that_is_no_longer_an_image_is_skipped() {
        let root = std::env::temp_dir().join(format!("hearth-attach-{}-gone2", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let workspace = root.join("ws");
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::write(workspace.join("shot.png"), "not an image").expect("text");
        let hook = parts_hook(&root, Some(workspace));
        // The declared media type would be a lie; the bytes decide.
        let parts = hook(&tool_message(
            serde_json::json!({"imagePath": "shot.png", "mediaType": "image/png"}),
        ));
        assert!(parts.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
