// The non-React half of the composer's file support: picked files in,
// `Attachment[]` out, with every cap enforced before a byte is read.
//
// It lives outside the component because these are the parts that can be wrong
// in ways a screenshot never shows:
//
//   * base64. `btoa` takes a *binary string*, and a PNG's bytes are not one;
//     handing bytes to btoa through a lossy conversion is the classic mojibake
//     bug that still decodes into something, so nothing complains until the
//     model sees a corrupt image. bytesToBase64() does the arithmetic itself
//     and is tested against strings computed by hand (attachments.test.ts).
//   * what a picked file *is*. The wire has three kinds; a file chosen from
//     disk has no workspace path, so it can only travel as `image` or `diff`
//     with `data`. Anything else is refused here, with a sentence that says
//     why — never dropped in silence.
//   * the caps. Checked against `file.size` *before* reading, so a 2 GB file
//     costs one refusal and not 2 GB of memory.
//
// Nothing here touches the DOM: the input type is the structural
// PickableFile, which `File` satisfies, so the tests build plain objects.
import type { AttachmentKind } from '../api/types';
import type { OutgoingAttachment } from '../api/client';

/** The largest file one attachment may carry, in bytes. */
export const MAX_ATTACHMENT_BYTES = 768 * 1024;

/**
 * The server's per-attachment cap, in *base64 characters*: `data.len() > 1 MiB`
 * at crates/server/src/server.rs:1957. It is measured on the encoded body, not
 * on the file, which is why MAX_ATTACHMENT_BYTES is 768 KiB and not 1 MiB:
 * base64 turns 3 bytes into 4 characters, so 786432 bytes encode to exactly
 * 1048576 characters and 786433 would be refused with a 422 the user could not
 * have predicted. base64Length() below is the arithmetic, asserted in the test.
 */
export const SERVER_MAX_ENCODED_BYTES = 1024 * 1024;

/**
 * The total the server accepts per message, in *decoded* bytes
 * (crates/server/src/server.rs:2001). It is the sum of the files, not of their
 * base64, so this is the number to compare a file's size against.
 */
export const MAX_TOTAL_BYTES = 8 * 1024 * 1024;

/** At most this many attachments in one message (server.rs:56). */
export const MAX_ATTACHMENTS = 10;

/** The limits addFiles() enforces. Injectable so a test need not build 768 KiB. */
export interface AttachmentLimits {
  perFile: number;
  total: number;
  count: number;
}

export const DEFAULT_ATTACHMENT_LIMITS: AttachmentLimits = {
  perFile: MAX_ATTACHMENT_BYTES,
  total: MAX_TOTAL_BYTES,
  count: MAX_ATTACHMENTS,
};

/**
 * One attachment the composer is holding, ready to send. `data` is base64 and
 * `bytes` is the decoded length — kept so the caps can be re-checked without
 * re-reading every file on each pick.
 */
export interface DraftAttachment {
  /** Identity for React keys and for removal; a pick has no other id. */
  id: string;
  /** `image` or `diff`: a picked file can never be `file` (it has no path). */
  kind: AttachmentKind;
  name: string;
  mediaType: string;
  bytes: number;
  data: string;
}

/** Why a file (or the selection itself) was not added. Rendered verbatim. */
export interface AttachmentRefusal {
  /** The file's name, or null when the refusal is about the selection itself. */
  name: string | null;
  /** One sentence naming the limit or the reason. */
  reason: string;
}

export interface AddFilesResult {
  added: DraftAttachment[];
  refused: AttachmentRefusal[];
  /** Decoded bytes held after this call: the caller does not recompute it. */
  totalBytes: number;
}

/** The part of `File` this module needs. `File` satisfies it structurally. */
export interface PickableFile {
  readonly name: string;
  readonly type: string;
  readonly size: number;
  arrayBuffer(): Promise<ArrayBuffer>;
}

const BASE64_ALPHABET = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';

/**
 * Encode bytes as standard, padded base64 — the alphabet and padding the
 * server's strict decoder accepts (server.rs:89).
 *
 * Written out rather than delegated to btoa because btoa consumes a *binary
 * string*: every byte above 0x7F has to survive a round trip through a string
 * first, and the usual shortcut (`String.fromCharCode(...bytes)`) silently
 * truncates in large chunks. Six bits at a time has no such failure mode.
 */
export function bytesToBase64(bytes: Uint8Array): string {
  const parts: string[] = [];
  const at = (index: number): number => bytes[index] ?? 0;
  let index = 0;
  for (; index + 2 < bytes.length; index += 3) {
    const triple = (at(index) << 16) | (at(index + 1) << 8) | at(index + 2);
    parts.push(
      BASE64_ALPHABET.charAt((triple >>> 18) & 63) +
        BASE64_ALPHABET.charAt((triple >>> 12) & 63) +
        BASE64_ALPHABET.charAt((triple >>> 6) & 63) +
        BASE64_ALPHABET.charAt(triple & 63),
    );
  }
  const rest = bytes.length - index;
  if (rest === 1) {
    const triple = at(index) << 16;
    parts.push(
      BASE64_ALPHABET.charAt((triple >>> 18) & 63) +
        BASE64_ALPHABET.charAt((triple >>> 12) & 63) +
        '==',
    );
  } else if (rest === 2) {
    const triple = (at(index) << 16) | (at(index + 1) << 8);
    parts.push(
      BASE64_ALPHABET.charAt((triple >>> 18) & 63) +
        BASE64_ALPHABET.charAt((triple >>> 12) & 63) +
        BASE64_ALPHABET.charAt((triple >>> 6) & 63) +
        '=',
    );
  }
  return parts.join('');
}

/** Base64 length for `byteCount` bytes: 4 characters per 3 bytes, rounded up. */
export function base64Length(byteCount: number): number {
  if (byteCount <= 0) return 0;
  return Math.ceil(byteCount / 3) * 4;
}

const UNITS = ['B', 'KiB', 'MiB', 'GiB'] as const;

/** A byte count a person reads: `768 KiB`, `8 MiB`, `1023 B`. */
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return `${String(bytes)} B`;
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < UNITS.length - 1) {
    value /= 1024;
    unit += 1;
  }
  const shown = unit === 0 ? String(value) : value.toFixed(1).replace(/\.0$/, '');
  return `${shown} ${UNITS[unit] ?? 'B'}`;
}

const IMAGE_EXTENSIONS: Readonly<Record<string, string>> = {
  png: 'image/png',
  jpg: 'image/jpeg',
  jpeg: 'image/jpeg',
  gif: 'image/gif',
  webp: 'image/webp',
  bmp: 'image/bmp',
  avif: 'image/avif',
};

const DIFF_MEDIA_TYPES: ReadonlySet<string> = new Set([
  'text/x-diff',
  'text/x-patch',
  'application/x-patch',
  'application/diff',
]);

/** The lowercased extension after the last dot, or '' when there is none. */
export function extensionOf(name: string): string {
  const dot = name.lastIndexOf('.');
  if (dot <= 0 || dot === name.length - 1) return '';
  return name.slice(dot + 1).toLowerCase();
}

/**
 * What a picked file travels as: `image`, `diff`, or null when it cannot be
 * sent at all.
 *
 * The media type decides first (that is what the OS actually reported), then
 * the extension, because a browser reports '' for plenty of real files and
 * `application/octet-stream` for a `.patch` it does not recognise. `file` is
 * never returned: that kind carries a workspace *path*, and a file chosen from
 * disk has none — sending its bytes under that kind would be a lie about where
 * they came from.
 */
export function classifyAttachment(mediaType: string, name: string): AttachmentKind | null {
  const type = mediaType.trim().toLowerCase();
  if (type.startsWith('image/') && type.length > 'image/'.length) return 'image';
  if (DIFF_MEDIA_TYPES.has(type)) return 'diff';
  const extension = extensionOf(name);
  if (Object.prototype.hasOwnProperty.call(IMAGE_EXTENSIONS, extension)) return 'image';
  if (extension === 'diff' || extension === 'patch') return 'diff';
  return null;
}

/** The media type to record: what the browser said, else inferred from the name. */
export function mediaTypeFor(kind: AttachmentKind, mediaType: string, name: string): string {
  const declared = mediaType.trim();
  if (declared !== '') return declared;
  if (kind === 'image') return IMAGE_EXTENSIONS[extensionOf(name)] ?? 'application/octet-stream';
  if (kind === 'diff') return 'text/x-diff';
  return 'application/octet-stream';
}

/** The wire entries for a set of drafts, in the order the user picked them. */
export function toAttachments(drafts: readonly DraftAttachment[]): OutgoingAttachment[] {
  return drafts.map((draft) => ({
    kind: draft.kind,
    data: draft.data,
    name: draft.name,
    mediaType: draft.mediaType,
  }));
}

/** Decoded bytes already held. */
export function totalBytesOf(drafts: readonly DraftAttachment[]): number {
  return drafts.reduce((sum, draft) => sum + draft.bytes, 0);
}

/** The drafts without `id`. Removal is by id, not by index, so it cannot slip. */
export function removeAttachment(
  drafts: readonly DraftAttachment[],
  id: string,
): DraftAttachment[] {
  return drafts.filter((draft) => draft.id !== id);
}

/** Ids only have to be unique inside one composer; they never leave the page. */
let nextAttachmentId = 0;

function newAttachmentId(): string {
  nextAttachmentId += 1;
  return `attachment-${String(nextAttachmentId)}`;
}

function tooLarge(name: string, bytes: number, limit: number): AttachmentRefusal {
  return {
    name,
    reason: `Too large: "${name}" is ${formatBytes(bytes)}; the limit is ${formatBytes(limit)} per attachment.`,
  };
}

function tooMuchTogether(name: string, total: number, limit: number): AttachmentRefusal {
  return {
    name,
    reason: `Too much at once: "${name}" would bring the attachments to ${formatBytes(total)}; the limit is ${formatBytes(limit)} in total.`,
  };
}

/**
 * Turn picked files into drafts, refusing — visibly and one by one — whatever
 * cannot be sent.
 *
 * The order of the checks is the point: size, kind, count and running total are
 * all decided from `file.size` and the name, so a file that cannot be sent is
 * never read. Only a file that passed every check reaches `arrayBuffer()`.
 *
 * One unreadable file does not lose the others, and nothing here throws: a
 * refusal is a value the component renders.
 */
export async function addFiles(
  files: readonly PickableFile[],
  existing: readonly DraftAttachment[] = [],
  limits: AttachmentLimits = DEFAULT_ATTACHMENT_LIMITS,
): Promise<AddFilesResult> {
  if (files.length === 0) {
    return {
      added: [],
      refused: [{ name: null, reason: 'No files were chosen, so nothing was attached.' }],
      totalBytes: totalBytesOf(existing),
    };
  }

  const added: DraftAttachment[] = [];
  const refused: AttachmentRefusal[] = [];
  let total = totalBytesOf(existing);

  for (const file of files) {
    if (existing.length + added.length >= limits.count) {
      refused.push({
        name: file.name,
        reason: `Too many attachments: one message may carry ${String(limits.count)}; "${file.name}" would be number ${String(existing.length + added.length + 1)}.`,
      });
      continue;
    }
    // Before any read: a 2 GB file must cost a sentence, not 2 GB of memory.
    if (file.size > limits.perFile) {
      refused.push(tooLarge(file.name, file.size, limits.perFile));
      continue;
    }
    if (file.size === 0) {
      refused.push({ name: file.name, reason: `Empty file: "${file.name}" has no bytes to attach.` });
      continue;
    }
    const kind = classifyAttachment(file.type, file.name);
    if (kind === null) {
      const described = file.type.trim() === '' ? 'not an image or a diff' : `type ${file.type}`;
      refused.push({
        name: file.name,
        reason: `Unsupported file: "${file.name}" is ${described}. Attach an image or a .diff/.patch file; a plain file must be named by a path inside the workspace, which a picked file cannot carry.`,
      });
      continue;
    }
    if (total + file.size > limits.total) {
      refused.push(tooMuchTogether(file.name, total + file.size, limits.total));
      continue;
    }

    let bytes: Uint8Array;
    try {
      bytes = new Uint8Array(await file.arrayBuffer());
    } catch (error) {
      const detail = error instanceof Error ? error.message : String(error);
      refused.push({
        name: file.name,
        reason: `Could not read "${file.name}": ${detail}. It was not attached.`,
      });
      continue;
    }
    // `size` is what the file system reported; the bytes are what arrived. The
    // caps above used the former, so re-check the latter rather than trusting a
    // number a browser or a sparse file could have got wrong.
    if (bytes.length > limits.perFile) {
      refused.push(tooLarge(file.name, bytes.length, limits.perFile));
      continue;
    }
    if (total + bytes.length > limits.total) {
      refused.push(tooMuchTogether(file.name, total + bytes.length, limits.total));
      continue;
    }
    total += bytes.length;
    added.push({
      id: newAttachmentId(),
      kind,
      name: file.name,
      mediaType: mediaTypeFor(kind, file.type, file.name),
      bytes: bytes.length,
      data: bytesToBase64(bytes),
    });
  }

  return { added, refused, totalBytes: total };
}