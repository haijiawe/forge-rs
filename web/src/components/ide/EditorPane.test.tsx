// The editor pane's reveal, which is the part of the pane no other test can see.
//
// Two facts have to hold when a search hit is opened, and both are timing facts
// rather than return values:
//
//   * Monaco is imported lazily, so the pane's first effects run while the
//     editor does not exist yet. The reveal must still happen once it does —
//     the failure this file was written for was a reveal that ran once against
//     `null` and then waited forever for a re-render that never came.
//   * The mark is not torn down when the store forgets the reveal. The parent
//     clears the pending reveal in response to `onRevealed`, so an unmounting
//     cleanup would make the highlight blink for one frame.
//
// jsdom cannot host Monaco (it has no canvas), so "./canvas" says yes and
// "./monacoSetup" is a fake that answers in a microtask — the same delay the
// real dynamic import has, which is the whole point.
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { mount, waitFor } from '../../test/render';
import { EditorPane } from './EditorPane';

const fake = vi.hoisted(() => {
  const calls = {
    revealed: [] as number[],
    positioned: [] as number[],
    /** One entry per collection created, holding its class names. */
    collections: [] as string[][],
    /** Ids of the collections that were cleared, in order. */
    cleared: [] as number[],
    created: 0,
  };

  let value = '';
  const instance = {
    onDidChangeModelContent: () => ({ dispose: () => {} }),
    addCommand: () => {},
    setValue: (text: string) => {
      value = text;
    },
    getValue: () => value,
    getModel: () => null,
    updateOptions: () => {},
    revealLineInCenter: (line: number) => {
      calls.revealed.push(line);
    },
    setPosition: (position: { lineNumber: number }) => {
      calls.positioned.push(position.lineNumber);
    },
    createDecorationsCollection: (items: { options?: { className?: string } }[]) => {
      calls.created += 1;
      const id = calls.created;
      calls.collections.push(items.map((item) => item.options?.className ?? ''));
      return {
        clear: () => {
          calls.cleared.push(id);
        },
      };
    },
    dispose: () => {},
  };

  const monaco = {
    editor: {
      create: () => instance,
      setModelLanguage: () => {},
    },
    KeyMod: { CtrlCmd: 1 },
    KeyCode: { KeyS: 2 },
  };

  return { calls, monaco };
});

vi.mock('./canvas', () => ({ canvasAvailableOnce: () => true }));
vi.mock('./monacoSetup', () => ({
  configureMonaco: () => {},
  applyTheme: () => {},
  monaco: fake.monaco,
  THEME: 'fake',
  languageFor: () => 'plaintext',
}));

const TEXT = Array.from({ length: 40 }, (_, index) => `line ${String(index + 1)}`).join('\n');
const OTHER = Array.from({ length: 60 }, (_, index) => `other ${String(index + 1)}`).join('\n');

beforeEach(() => {
  fake.calls.revealed.length = 0;
  fake.calls.positioned.length = 0;
  fake.calls.collections.length = 0;
  fake.calls.cleared.length = 0;
  fake.calls.created = 0;
});

describe('EditorPane reveal', () => {
  it('scrolls to the hit once the lazily imported editor exists', async () => {
    const onRevealed = vi.fn();
    const view = mount(
      <EditorPane
        path="src/main.ts"
        text={TEXT}
        onChange={() => {}}
        revealLine={7}
        onRevealed={onRevealed}
      />,
    );

    // The pane has mounted; the editor module has not answered yet. A reveal
    // consumed here would scroll an editor that does not exist.
    expect(fake.calls.revealed).toEqual([]);

    await waitFor(() => fake.calls.revealed.length > 0);

    expect(fake.calls.revealed).toEqual([7]);
    expect(fake.calls.positioned).toEqual([7]);
    expect(fake.calls.collections).toEqual([['wb-line-revealed']]);
    expect(onRevealed).toHaveBeenCalledTimes(1);
    view.unmount();
  });

  it('keeps the mark on screen after the store forgets the reveal', async () => {
    const onRevealed = vi.fn();
    const view = mount(
      <EditorPane
        path="src/main.ts"
        text={TEXT}
        onChange={() => {}}
        revealLine={7}
        onRevealed={onRevealed}
      />,
    );
    await waitFor(() => fake.calls.collections.length === 1);

    // What the parent does when onRevealed fires: the pending reveal is
    // cleared, so this effect re-runs with no line to reveal.
    view.rerender(
      <EditorPane
        path="src/main.ts"
        text={TEXT}
        onChange={() => {}}
        revealLine={null}
        onRevealed={onRevealed}
      />,
    );

    expect(fake.calls.cleared).toEqual([]);
    view.unmount();
  });

  it('drops the old mark when the next hit is in another file', async () => {
    const onRevealed = vi.fn();
    const view = mount(
      <EditorPane
        path="a.ts"
        text={TEXT}
        onChange={() => {}}
        revealLine={3}
        onRevealed={onRevealed}
      />,
    );
    await waitFor(() => fake.calls.collections.length === 1);

    view.rerender(
      <EditorPane
        path="b.ts"
        text={OTHER}
        onChange={() => {}}
        revealLine={9}
        onRevealed={onRevealed}
      />,
    );
    await waitFor(() => fake.calls.collections.length === 2);

    // Only the mark that described a.ts was cleared. A new file's mark cleared
    // by the previous file's cleanup is the same as no mark at all.
    expect(fake.calls.cleared).toEqual([1]);
    expect(fake.calls.collections[1]).toEqual(['wb-line-revealed']);
    expect(fake.calls.revealed).toEqual([3, 9]);
    view.unmount();
  });
});
