// What the last turn could actually call.
//
// The tool list is not invented here: it is read straight out of the most
// recent `llm/call` event, whose `tools` member is the set of names that went
// into the request (docs/contracts/events-v1.md §4). That is the only truthful
// source in the UI — the browser is not allowed to ask the harness what is
// configured (no endpoint exists for it), and a panel built from a guess would
// be worse than no panel.
//
// Two rules keep this honest:
//
//  * A session that has not sent a request yet has no list. The panel says so;
//    it never renders an empty table as if the harness had no tools.
//  * Names are grouped by their own shape, not by a table of names this file
//    hopes exist. A tool the browser has never heard of lands in `other` and is
//    still shown — dropping it would make the panel lie by omission.

/** One tool, as the request named it. */
export interface Capability {
  /** The exact name from the event. Never rewritten. */
  name: string;
  /** Group key, from the shape of the name. */
  group: CapabilityGroup;
}

export type CapabilityGroup =
  | 'files'
  | 'process'
  | 'notes'
  | 'network'
  | 'servers'
  | 'plugins'
  | 'platform'
  | 'delegation'
  | 'skills'
  | 'other';

export interface CapabilitySection {
  group: CapabilityGroup;
  /** Heading shown to the operator. */
  title: string;
  /** One line saying what this group means; wording is shared with the docs. */
  detail: string;
  tools: Capability[];
}

const TITLES: Record<CapabilityGroup, { title: string; detail: string }> = {
  files: {
    title: 'files',
    detail: 'read, write, search inside the workspace',
  },
  process: {
    title: 'process',
    detail: 'runs under the sandbox; approval depends on the mode',
  },
  notes: {
    title: 'notes',
    detail: 'the model\u2019s own bookkeeping',
  },
  network: {
    title: 'network',
    detail: 'runs inside the harness, so the sandbox cannot contain it',
  },
  servers: {
    title: 'mcp servers',
    detail: 'remote tools; the operator approves each call',
  },
  plugins: {
    title: 'plugins',
    detail: 'child processes speaking the plugin protocol',
  },
  platform: {
    title: 'platform (experts & teams)',
    detail: 'P3: summons an expert from the roster, or dispatches a team over a board',
  },
  delegation: {
    title: 'delegation',
    detail: 'spends more model calls than this turn',
  },
  skills: {
    title: 'skills',
    detail: 'installed documents the model may open',
  },
  other: {
    title: 'other',
    detail: 'names this build does not classify; shown rather than hidden',
  },
};

/** The order sections appear in; stable across tools being added or removed. */
const ORDER: CapabilityGroup[] = [
  'files',
  'process',
  'notes',
  'network',
  'servers',
  'plugins',
  'platform',
  'delegation',
  'skills',
  'other',
];

const PREFIX_MCP = 'mcp__';
const SEPARATOR_PLUGIN = '.';

/**
 * Group a tool name by its shape.
 *
 * Two names need no table: an MCP tool is `mcp__server__tool` and a plugin tool
 * is `plugin.tool`, both forms imposed by the wiring layer rather than names it
 * chose. The P3 platform tools (`expert`, `team`) are a fixed pair the CLI wires
 * in, and are kept apart from the rest because they are the only two that say
 * anything about the expert roster and the team board. Everything else is the
 * builtin vocabulary; a name this file has never heard of is `other`.
 */
export function groupOf(name: string): CapabilityGroup {
  if (name.startsWith(PREFIX_MCP)) return 'servers';
  if (name.includes(SEPARATOR_PLUGIN)) return 'plugins';
  switch (name) {
    case 'fs_read':
    case 'fs_write':
    case 'fs_edit':
    case 'fs_glob':
    case 'fs_grep':
      return 'files';
    case 'shell':
      return 'process';
    case 'worktree':
      return 'delegation';
    case 'todo_write':
    case 'present':
    case 'goal_end':
      return 'notes';
    case 'web_fetch':
    case 'web_search':
      return 'network';
    case 'skill':
      return 'skills';
    case 'expert':
    case 'team':
      return 'platform';
    case 'subagent':
      return 'delegation';
    default:
      return 'other';
  }
}

/** Group the names from one request into sections, empty groups omitted. */
export function sectionsFor(names: readonly string[]): CapabilitySection[] {
  const byGroup = new Map<CapabilityGroup, Capability[]>();
  for (const raw of names) {
    const name = raw.trim();
    if (name === '') continue;
    const group = groupOf(name);
    const list = byGroup.get(group) ?? [];
    list.push({ name, group });
    byGroup.set(group, list);
  }
  const sections: CapabilitySection[] = [];
  for (const group of ORDER) {
    const tools = byGroup.get(group);
    if (tools === undefined || tools.length === 0) continue;
    // Sorted by name: a request that gained a tool should not reshuffle the
    // reader's picture of the ones that were already there.
    tools.sort((a, b) => a.name.localeCompare(b.name));
    sections.push({ group, tools, ...TITLES[group] });
  }
  return sections;
}

/**
 * The tool list of the newest `llm/call` event, or an empty list.
 *
 * Later events win because the list can change mid-session (a plugin can be
 * loaded between turns), and the panel describes "the last request" rather than
 * "the union of everything ever offered".
 */
export function latestToolNames(events: readonly { type: string; payload?: unknown }[]): string[] {
  for (let index = events.length - 1; index >= 0; index -= 1) {
    const event = events[index];
    if (event === undefined || event.type !== 'llm/call') continue;
    const payload = event.payload;
    if (typeof payload !== 'object' || payload === null) continue;
    const tools = (payload as { tools?: unknown }).tools;
    if (!Array.isArray(tools)) continue;
    const names: string[] = [];
    for (const entry of tools) {
      if (typeof entry === 'string') names.push(entry);
    }
    return names;
  }
  return [];
}
