// The composer's file support, tested without a DOM: this file builds plain
// objects that satisfy PickableFile, so it needs neither jsdom nor a real file
// input. What it pins is the part a browser cannot show you is wrong — a base64
// string that decodes into a corrupt image, and a 2 GB file that is read before
// it is refused.
import { describe, expect, it } from 'vitest';
import {
  addFiles,
  base64Length,
  bytesToBase64,
  classifyAttachment,
  extensionOf,
  formatBytes,
  MAX_ATTACHMENTS,
  MAX_ATTACHMENT_BYTES,
  MAX_TOTAL_BYTES,
  mediaTypeFor,
  removeAttachment,
  SERVER_MAX_ENCODED_BYTES,
  toAttachments,
  totalBytesOf,
  type AttachmentLimits,
  type DraftAttachment,
  type PickableFile,
} from './attachments';

/** A file the composer can pick: real bytes, a declared size, and a read count. */
class FakeFile implements PickableFile {
  readonly name: string;
  readonly type: string;
  size: number;
  reads = 0;
  private readonly bytes: Uint8Array;

  constructor(name: string, bytes: Uint8Array, type = '', declaredSize?: number) {
    this.name = name;
    this.bytes = bytes;
    this.type = type;
    this.size = declaredSize ?? bytes.length;
  }

  async arrayBuffer(): Promise<ArrayBuffer> {
    this.reads += 1;
    const copy = new Uint8Array(this.bytes.length);
    copy.set(this.bytes);
    return copy.buffer;
  }
}

/** A file whose read fails, to prove one bad file does not lose the others. */
class UnreadableFile extends FakeFile {
  override async arrayBuffer(): Promise<ArrayBuffer> {
    this.reads += 1;
    throw new Error('the file moved');
  }
}

const PNG_MAGIC = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);

function png(name = 'shot.png'): FakeFile {
  return new FakeFile(name, PNG_MAGIC, 'image/png');
}

function zeros(count: number): Uint8Array {
  return new Uint8Array(count);
}

const LOOSE: AttachmentLimits = {
  perFile: MAX_TOTAL_BYTES,
  total: MAX_TOTAL_BYTES,
  count: MAX_ATTACHMENTS,
};

function draftOf(bytes: number): DraftAttachment {
  return {
    id: `existing-${String(bytes)}`,
    kind: 'image',
    name: 'already.png',
    mediaType: 'image/png',
    bytes,
    data: '',
  };
}

describe('bytesToBase64', () => {
  // Every expected string below was computed by hand from the bit pattern, not
  // produced by the implementation: a test that agrees with the code it tests
  // proves only that the code is deterministic.
  it('encodes the bytes of a PNG header', () => {
    // 10001001 01010000 01001110 -> 100010 010101 000001 001110 -> i V B O
    expect(bytesToBase64(new Uint8Array([0x89, 0x50, 0x4e, 0x47]))).toBe('iVBORw==');
    expect(bytesToBase64(PNG_MAGIC)).toBe('iVBORw0KGgo=');
  });

  it('is correct for bytes at and above 0x80', () => {
    // The byte a string round trip corrupts: 0x89 alone is 100010 01 -> i Q.
    expect(bytesToBase64(new Uint8Array([0x89]))).toBe('iQ==');
    expect(bytesToBase64(new Uint8Array([0xff]))).toBe('/w==');
    expect(bytesToBase64(new Uint8Array([0xff, 0xfe, 0xfd]))).toBe('//79');
    expect(bytesToBase64(new Uint8Array([0xff, 0xff]))).toBe('//8=');
    expect(bytesToBase64(new Uint8Array([0xc3, 0xa9]))).toBe('w6k=');
  });

  it('pads the two remainder lengths correctly', () => {
    expect(bytesToBase64(new Uint8Array(0))).toBe('');
    expect(bytesToBase64(new Uint8Array([0x61]))).toBe('YQ==');
    expect(bytesToBase64(new Uint8Array([0x61, 0x62]))).toBe('YWI=');
    expect(bytesToBase64(new Uint8Array([0x61, 0x62, 0x63]))).toBe('YWJj');
  });

  it('produces a string the platform decoder reads back unchanged', () => {
    const bytes = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0xff, 0x00]);
    const decoded = atob(bytesToBase64(bytes));
    const expected = Array.from(bytes, (byte) => String.fromCharCode(byte)).join('');
    expect(decoded).toBe(expected);
  });
});

describe('base64Length', () => {
  it('counts four characters per three bytes, rounded up', () => {
    expect(base64Length(0)).toBe(0);
    expect(base64Length(1)).toBe(4);
    expect(base64Length(3)).toBe(4);
    expect(base64Length(4)).toBe(8);
    expect(base64Length(MAX_ATTACHMENT_BYTES)).toBe(SERVER_MAX_ENCODED_BYTES);
  });

  it('is why the per-file cap is 768 KiB and not 1 MiB', () => {
    // The server's limit is on the encoded string (server.rs:1957), so the
    // largest file it accepts is the largest byte count whose base64 fits.
    expect(base64Length(MAX_ATTACHMENT_BYTES)).toBe(1048576);
    expect(base64Length(MAX_ATTACHMENT_BYTES + 1)).toBeGreaterThan(SERVER_MAX_ENCODED_BYTES);
  });
});

describe('formatBytes', () => {
  it('names the size a person reads', () => {
    expect(formatBytes(0)).toBe('0 B');
    expect(formatBytes(1023)).toBe('1023 B');
    expect(formatBytes(1024)).toBe('1 KiB');
    expect(formatBytes(1536)).toBe('1.5 KiB');
    expect(formatBytes(MAX_ATTACHMENT_BYTES)).toBe('768 KiB');
    expect(formatBytes(MAX_TOTAL_BYTES)).toBe('8 MiB');
    expect(formatBytes(3 * 1024 * 1024)).toBe('3 MiB');
    expect(formatBytes(2 * 1024 * 1024 * 1024)).toBe('2 GiB');
  });
});

describe('classifyAttachment', () => {
  it('lets the declared media type decide first', () => {
    expect(classifyAttachment('image/png', 'shot.bin')).toBe('image');
    expect(classifyAttachment('image/svg+xml', 'drawing')).toBe('image');
    expect(classifyAttachment('text/x-diff', 'change.txt')).toBe('diff');
    expect(classifyAttachment('application/x-patch', 'change')).toBe('diff');
  });

  it('falls back to the extension when the browser declares nothing useful', () => {
    expect(classifyAttachment('', 'shot.PNG')).toBe('image');
    expect(classifyAttachment('application/octet-stream', 'shot.jpeg')).toBe('image');
    expect(classifyAttachment('application/octet-stream', 'change.diff')).toBe('diff');
    expect(classifyAttachment('', 'change.patch')).toBe('diff');
  });

  it('never returns file: a picked file has no workspace path', () => {
    for (const name of ['notes.txt', 'archive.zip', 'Cargo.toml', 'no-extension']) {
      expect(classifyAttachment('application/octet-stream', name)).toBeNull();
    }
    // A bare "image/" is not an image type; with no image extension there is
    // nothing left to classify it by.
    expect(classifyAttachment('image/', 'mystery')).toBeNull();
  });
});

describe('extensionOf', () => {
  it('reads the extension after the last dot, lowercased', () => {
    expect(extensionOf('shot.PNG')).toBe('png');
    expect(extensionOf('a.b.patch')).toBe('patch');
    expect(extensionOf('no-extension')).toBe('');
    expect(extensionOf('.gitignore')).toBe('');
    expect(extensionOf('trailing.')).toBe('');
  });
});

describe('mediaTypeFor', () => {
  it('keeps what the browser declared', () => {
    expect(mediaTypeFor('image', 'image/png', 'shot.png')).toBe('image/png');
  });

  it('infers one when the browser declared nothing', () => {
    expect(mediaTypeFor('image', '', 'shot.webp')).toBe('image/webp');
    expect(mediaTypeFor('diff', '', 'change.patch')).toBe('text/x-diff');
    expect(mediaTypeFor('image', '', 'no-extension')).toBe('application/octet-stream');
  });
});

describe('addFiles', () => {
  it('turns a picked PNG into an attachment with its bytes as base64', async () => {
    const result = await addFiles([png()]);

    expect(result.refused).toEqual([]);
    expect(result.totalBytes).toBe(PNG_MAGIC.length);
    expect(result.added).toHaveLength(1);
    const [added] = result.added;
    expect(added).toBeDefined();
    expect(added?.kind).toBe('image');
    expect(added?.name).toBe('shot.png');
    expect(added?.mediaType).toBe('image/png');
    expect(added?.bytes).toBe(8);
    expect(added?.data).toBe('iVBORw0KGgo=');
  });

  it('gives each pick its own id so two files can be removed separately', async () => {
    const result = await addFiles([png('a.png'), png('b.png')]);
    const ids = result.added.map((draft) => draft.id);
    expect(ids).toHaveLength(2);
    expect(new Set(ids).size).toBe(2);
  });

  it('refuses a 2 GB file without reading it', async () => {
    const huge = new FakeFile('video.png', zeros(4), 'image/png', 2 * 1024 * 1024 * 1024);

    const result = await addFiles([huge]);

    expect(result.added).toEqual([]);
    expect(result.refused).toHaveLength(1);
    expect(result.refused[0]?.reason).toContain('Too large: "video.png" is 2 GiB');
    expect(result.refused[0]?.reason).toContain('768 KiB per attachment');
    // The point of checking `size` first: nothing was read into memory.
    expect(huge.reads).toBe(0);
  });

  it('names the per-attachment limit for a file just over it', async () => {
    const over = new FakeFile('big.png', zeros(1), 'image/png', MAX_ATTACHMENT_BYTES + 1);

    const result = await addFiles([over]);

    expect(result.refused[0]?.reason).toBe(
      'Too large: "big.png" is 768 KiB; the limit is 768 KiB per attachment.',
    );
    // 768 KiB + 1 byte rounds to 768 KiB at one decimal; the refusal is still
    // correct, and the number the user can act on is the limit.
    expect(over.reads).toBe(0);
  });

  it('refuses the file that would break the total, and keeps the ones before it', async () => {
    const limits: AttachmentLimits = { perFile: 4 * 1024 * 1024, total: 4 * 1024 * 1024, count: 10 };
    const first = new FakeFile('first.png', zeros(3 * 1024 * 1024), 'image/png');
    const second = new FakeFile('second.png', zeros(2 * 1024 * 1024), 'image/png');

    const result = await addFiles([first, second], [], limits);

    expect(result.added.map((draft) => draft.name)).toEqual(['first.png']);
    expect(result.refused[0]?.reason).toBe(
      'Too much at once: "second.png" would bring the attachments to 5 MiB; the limit is 4 MiB in total.',
    );
    expect(result.totalBytes).toBe(3 * 1024 * 1024);
  });

  it('counts the attachments already picked against the total', async () => {
    const existing = [draftOf(7 * 1024 * 1024)];
    const extra = new FakeFile('extra.png', zeros(2 * 1024 * 1024), 'image/png');

    const result = await addFiles([extra], existing, LOOSE);

    expect(result.added).toEqual([]);
    expect(result.refused[0]?.reason).toContain('would bring the attachments to 9 MiB');
    expect(extra.reads).toBe(0);
  });

  it('refuses more than the server can carry in one message', async () => {
    const limits: AttachmentLimits = { perFile: MAX_ATTACHMENT_BYTES, total: MAX_TOTAL_BYTES, count: 2 };

    const result = await addFiles([png('a.png'), png('b.png'), png('c.png')], [], limits);

    expect(result.added.map((draft) => draft.name)).toEqual(['a.png', 'b.png']);
    expect(result.refused[0]?.reason).toBe(
      'Too many attachments: one message may carry 2; "c.png" would be number 3.',
    );
  });

  it('refuses an unsupported file with a sentence that offers the alternative', async () => {
    const notes = new FakeFile('notes.txt', new Uint8Array([0x68, 0x69]), 'text/plain');

    const result = await addFiles([notes]);

    expect(result.added).toEqual([]);
    expect(result.refused[0]?.name).toBe('notes.txt');
    expect(result.refused[0]?.reason).toBe(
      'Unsupported file: "notes.txt" is type text/plain. Attach an image or a .diff/.patch file; a plain file must be named by a path inside the workspace, which a picked file cannot carry.',
    );
    expect(notes.reads).toBe(0);
  });

  it('refuses an empty file', async () => {
    const empty = new FakeFile('empty.png', new Uint8Array(0), 'image/png');

    const result = await addFiles([empty]);

    expect(result.added).toEqual([]);
    expect(result.refused[0]?.reason).toBe('Empty file: "empty.png" has no bytes to attach.');
  });

  it('says so when nothing was chosen', async () => {
    const result = await addFiles([]);

    expect(result.added).toEqual([]);
    expect(result.refused[0]?.reason).toBe('No files were chosen, so nothing was attached.');
  });

  it('keeps the good files when one of them cannot be read', async () => {
    const broken = new UnreadableFile('broken.png', zeros(4), 'image/png');

    const result = await addFiles([broken, png('good.png')]);

    expect(result.added.map((draft) => draft.name)).toEqual(['good.png']);
    expect(result.refused[0]?.name).toBe('broken.png');
    expect(result.refused[0]?.reason).toBe(
      'Could not read "broken.png": the file moved. It was not attached.',
    );
  });

  it('re-checks the bytes that arrived, not the size the file system reported', async () => {
    // A sparse or lying file: it claims to be tiny and delivers 1 MiB.
    const liar = new FakeFile('liar.png', zeros(MAX_ATTACHMENT_BYTES + 1), 'image/png', 4);

    const result = await addFiles([liar]);

    expect(result.added).toEqual([]);
    expect(result.refused[0]?.reason).toContain('Too large');
    expect(liar.reads).toBe(1);
  });
});

describe('toAttachments', () => {
  it('emits exactly the members the request body carries', async () => {
    const result = await addFiles([png()]);

    expect(toAttachments(result.added)).toEqual([
      { kind: 'image', data: 'iVBORw0KGgo=', name: 'shot.png', mediaType: 'image/png' },
    ]);
  });

  it('emits nothing for an empty draft', () => {
    expect(toAttachments([])).toEqual([]);
  });
});

describe('totalBytesOf and removeAttachment', () => {
  it('sums the decoded sizes', () => {
    expect(totalBytesOf([])).toBe(0);
    expect(totalBytesOf([draftOf(10), draftOf(32)])).toBe(42);
  });

  it('removes one attachment by id and leaves the rest', async () => {
    const result = await addFiles([png('a.png'), png('b.png')]);
    const [first] = result.added;
    expect(first).toBeDefined();
    if (first === undefined) return;

    const left = removeAttachment(result.added, first.id);

    expect(left.map((draft) => draft.name)).toEqual(['b.png']);
    // Removing something that is not there is a no-op, not an error.
    expect(removeAttachment(result.added, 'nothing')).toHaveLength(2);
  });
});