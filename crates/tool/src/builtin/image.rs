//! `read_image`: show the model a file that happens to be a picture.
//!
//! Ported in spirit from DSH's `read_image`, which this tree lacked. The shape is
//! forced by the transport rather than chosen: a tool result is a *text* message
//! on every chat-completions wire, so the bytes cannot ride there. They leave
//! through [`crate::Result::parts`], the loop turns them into a follow-up user
//! message, and the log records only a path and a size — the same discipline
//! attachments follow (`docs/contracts/events-v1.md` §4.0.1).
//!
//! The consequence worth stating: what the model sees next turn is re-read from
//! disk by the same history hook, so an image stays visible for the rest of the
//! session without a base64 copy of it in the log. If the file is deleted, the
//! history loses the image and says so in the tool output that named it.

use std::fs;
use std::path::Path;

use serde::Deserialize;
use serde_json::json;

use crate::builtin::fs::CODE_BINARY;
use crate::contract::{
    decode_args, Call, Result, CODE_DENIED, CODE_IO_ERROR, CODE_NOT_FOUND, CODE_OUTSIDE_WORKSPACE,
    ERR_BAD_ARGS,
};
use crate::Tool;

/// Refuse anything above this; a large PNG at 4 bytes per pixel base64-expands to
/// roughly a third of a megabyte of request per screenshot.
pub const MAX_IMAGE_BYTES: u64 = 8 << 20;

/// The image families a chat-completions message can express. Listed as a
/// denylist-free allowlist because a wrong `media_type` is a rejected request,
/// not a degraded one.
pub const ALLOWED_MEDIA_TYPES: [&str; 7] = [
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
    "image/bmp",
    "image/tiff",
    "image/x-icon",
];

pub struct ReadImageTool;

#[derive(Deserialize)]
struct ReadImageArgs {
    path: String,
}

impl Tool for ReadImageTool {
    fn spec(&self) -> hearth_llm::ToolSpec {
        hearth_llm::ToolSpec {
            name: "read_image".into(),
            description: format!(
                "Read an image file from the workspace so you can see it. \
                 Supports {} up to {} MiB. The bytes are shown to you once: later \
                 turns re-read the same file, so do not read it twice just to \
                 remember it. Example: {{\"path\":\"docs/screenshot.png\"}}",
                ALLOWED_MEDIA_TYPES.join(", "),
                MAX_IMAGE_BYTES / (1 << 20)
            ),
            schema: json!({
                "type":"object",
                "properties":{
                    "path":{"type":"string","description":"Image path relative to the workspace root."}
                },
                "required":["path"],
                "additionalProperties":false
            }),
        }
    }

    fn call(&self, call: Call) -> Result {
        let args: ReadImageArgs = match decode_args(&call) {
            Ok(args) => args,
            Err(result) => return result,
        };
        if args.path.trim().is_empty() {
            return Result::error(
                ERR_BAD_ARGS,
                "read_image: path is required (a path relative to the workspace root)",
            );
        }

        let absolute = match crate::path::resolve(&call.workspace, &args.path) {
            Ok(path) => path,
            Err(err) if crate::path::is_outside(&err) => {
                return Result::error(
                    CODE_OUTSIDE_WORKSPACE,
                    format!(
                        "read_image: path {:?} is outside the workspace; use a path relative to the workspace root ({})",
                        args.path, call.workspace
                    ),
                )
            }
            Err(err) => {
                return Result::error(
                    CODE_IO_ERROR,
                    format!("read_image: path {:?}: {err}", args.path),
                )
            }
        };

        let meta = match fs::metadata(&absolute) {
            Ok(meta) => meta,
            Err(err) => return path_error(&args.path, err),
        };
        if !meta.is_file() {
            return Result::error(
                CODE_BINARY,
                format!("read_image: {:?} is not a regular file", args.path),
            );
        }
        if meta.len() > MAX_IMAGE_BYTES {
            return Result::error(
                ERR_BAD_ARGS,
                format!(
                    "read_image: {:?} is {} bytes, above the {} byte limit; downscale it first",
                    args.path,
                    meta.len(),
                    MAX_IMAGE_BYTES
                ),
            );
        }

        let media_type = match sniff_media_type(&absolute) {
            Some(media_type) => media_type,
            None => {
                return Result::error(
                    CODE_BINARY,
                    format!(
                        "read_image: {:?} does not look like an image (want one of {}); use fs_read for text",
                        args.path,
                        ALLOWED_MEDIA_TYPES.join(", ")
                    ),
                )
            }
        };

        let data = match fs::read(&absolute) {
            Ok(data) => data,
            Err(err) => return path_error(&args.path, err),
        };
        let relative = crate::path::rel(&call.workspace, &absolute)
            .unwrap_or_else(|| absolute.to_string_lossy().replace('\\', "/"));

        // The output line is what a later turn re-reads: it names the file, so the
        // hook can find it again without the bytes being anywhere in the log.
        Result::ok(format!(
            "read_image: {relative} ({media_type}, {} bytes)",
            data.len()
        ))
        .with_meta("imagePath", json!(relative))
        .with_meta("mediaType", json!(media_type))
        .with_meta("bytes", json!(data.len() as u64))
        .with_parts(vec![hearth_llm::ContentPart::image(media_type, data)])
    }
}

fn path_error(path: &str, err: std::io::Error) -> Result {
    match err.kind() {
        std::io::ErrorKind::NotFound => {
            Result::error(CODE_NOT_FOUND, format!("read_image: {path:?} not found"))
        }
        std::io::ErrorKind::PermissionDenied => Result::error(
            CODE_DENIED,
            format!("read_image: {path:?}: permission denied"),
        ),
        _ => Result::error(CODE_IO_ERROR, format!("read_image: {path:?}: {err}")),
    }
}

/// Detect the image family from the first bytes.
///
/// Chosen over trusting the extension because the extension is a claim the model
/// can get wrong, and a wrong `media_type` is a rejected request rather than a
/// degraded one. Only formats on the allowlist are reported, so this doubles as
/// the "is this an image at all" answer.
pub fn sniff_media_type(path: &Path) -> Option<&'static str> {
    let bytes = fs::read(path).ok()?;
    Some(match bytes.as_slice() {
        [0x89, b'P', b'N', b'G', ..] => "image/png",
        [0xFF, 0xD8, 0xFF, ..] => "image/jpeg",
        [b'G', b'I', b'F', b'8', ..] => "image/gif",
        [b'B', b'M', ..] => "image/bmp",
        [b'I', b'I', 0x2A, 0x00, ..] | [b'M', b'M', 0x00, 0x2A, ..] => "image/tiff",
        [0x00, 0x00, 0x01, 0x00, ..] => "image/x-icon",
        // WebP is `RIFF....WEBP`; the fourcc sits at offset 8.
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => "image/webp",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

    fn scratch(label: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("hearth-read-image-{}-{label}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create the scratch directory");
        dir
    }

    fn call(dir: &Path, path: &str) -> Call {
        Call::new(
            "c1",
            "read_image",
            json!({"path": path}),
            dir.to_string_lossy().to_string(),
        )
    }

    #[test]
    fn a_png_becomes_one_image_part_and_the_output_names_it() {
        let dir = scratch("png");
        fs::write(dir.join("shot.png"), PNG).expect("write");
        let result = ReadImageTool.call(call(&dir, "shot.png"));
        assert!(!result.is_error, "{}", result.output);
        assert_eq!(result.output, "read_image: shot.png (image/png, 8 bytes)");
        assert_eq!(result.parts.len(), 1);
        match &result.parts[0] {
            hearth_llm::ContentPart::Image { media_type, data } => {
                assert_eq!(media_type, "image/png");
                assert_eq!(data.as_slice(), PNG);
            }
            other => panic!("expected an image part, got {other:?}"),
        }
        // The metadata a later turn needs to find the file again.
        assert_eq!(result.meta.get("imagePath").unwrap(), &json!("shot.png"));
        assert_eq!(result.meta.get("bytes").unwrap(), &json!(8));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_log_line_carries_no_bytes() {
        // The result is what the loop writes into the log; parts are `serde(skip)`.
        let dir = scratch("log");
        fs::write(dir.join("shot.png"), PNG).expect("write");
        let result = ReadImageTool.call(call(&dir, "shot.png"));
        let encoded = serde_json::to_string(&result).expect("serialize");
        assert!(
            !encoded.contains("iVBOR"),
            "base64 leaked into the result: {encoded}"
        );
        assert!(!encoded.contains("\"parts\""));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unknown_extension_with_image_bytes_is_read() {
        // The sniff decides, not the name: a PNG saved as `.dat` is still a PNG.
        let dir = scratch("sniff");
        fs::write(dir.join("mystery.dat"), PNG).expect("write");
        let result = ReadImageTool.call(call(&dir, "mystery.dat"));
        assert!(!result.is_error, "{}", result.output);
        assert!(result.output.contains("image/png"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_named_image_that_is_really_text_is_refused_with_a_usable_next_step() {
        let dir = scratch("text");
        fs::write(dir.join("fake.png"), b"not a picture at all").expect("write");
        let result = ReadImageTool.call(call(&dir, "fake.png"));
        assert!(result.is_error);
        assert_eq!(result.code, CODE_BINARY);
        assert!(result.output.contains("use fs_read for text"));
        assert!(result.parts.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_above_the_limit_is_refused_before_being_read() {
        let dir = scratch("big");
        let mut data = Vec::from(PNG);
        data.resize((MAX_IMAGE_BYTES + 1) as usize, 0);
        fs::write(dir.join("big.png"), &data).expect("write");
        let result = ReadImageTool.call(call(&dir, "big.png"));
        assert!(result.is_error);
        assert_eq!(result.code, ERR_BAD_ARGS);
        assert!(result.output.contains("downscale it first"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_path_outside_the_workspace_is_refused_before_being_opened() {
        let dir = scratch("outside");
        let result = ReadImageTool.call(call(&dir, "../elsewhere.png"));
        assert!(result.is_error);
        assert_eq!(result.code, CODE_OUTSIDE_WORKSPACE);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_file_says_so_with_not_found() {
        let dir = scratch("missing");
        let result = ReadImageTool.call(call(&dir, "nope.png"));
        assert!(result.is_error);
        assert_eq!(result.code, CODE_NOT_FOUND);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_directory_is_not_a_file() {
        let dir = scratch("dir");
        fs::create_dir_all(dir.join("pictures")).expect("create");
        let result = ReadImageTool.call(call(&dir, "pictures"));
        assert!(result.is_error);
        assert_eq!(result.code, CODE_BINARY);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_sniff_recognises_every_allowed_family() {
        assert_eq!(sniff_media_type_bytes(&PNG), Some("image/png"));
        assert_eq!(
            sniff_media_type_bytes(&[0xFF, 0xD8, 0xFF, 0xE0]),
            Some("image/jpeg")
        );
        assert_eq!(
            sniff_media_type_bytes(&[b'G', b'I', b'F', b'8', 0x39, 0x61]),
            Some("image/gif")
        );
        assert_eq!(
            sniff_media_type_bytes(&[b'B', b'M', 0, 0]),
            Some("image/bmp")
        );
        assert_eq!(
            sniff_media_type_bytes(&[b'I', b'I', 0x2A, 0x00]),
            Some("image/tiff")
        );
        assert_eq!(
            sniff_media_type_bytes(&[0x00, 0x00, 0x01, 0x00]),
            Some("image/x-icon")
        );
        assert_eq!(
            sniff_media_type_bytes(&[b'R', b'I', b'F', b'F', 0, 0, 0, 0, b'W', b'E', b'B', b'P']),
            Some("image/webp")
        );
        assert_eq!(sniff_media_type_bytes(b"hello"), None);
    }

    /// The sniff, applied to bytes rather than to a path.
    fn sniff_media_type_bytes(bytes: &[u8]) -> Option<&'static str> {
        Some(match bytes {
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
}
