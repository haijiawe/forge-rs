// `GET /api/capabilities`: what this process was configured with.
//
// The tools-in-the-last-request panel reads the session's own event log and so
// answers "what reached the model". It cannot answer "what is installed but was
// not offered" — that is configuration, and configuration is not in the log.
// This module is the other half: one GET at startup, decoded defensively,
// because a capability pane must never be the reason a page fails to render.

/** One configured capability, active or not. */
export interface CapabilityEntry {
  name: string;
  detail: string;
  /** True when this build offers it to the model right now. */
  active: boolean;
}

/** The whole snapshot, as the server sends it. */
export interface CapabilityReport {
  mode: string;
  workspace: string;
  model: string;
  plugins: CapabilityEntry[];
  experts: CapabilityEntry[];
  teams: CapabilityEntry[];
  bundles: CapabilityEntry[];
  skills: CapabilityEntry[];
  mcp: CapabilityEntry[];
}

/** One group of entries, ready to render. */
export interface ConfiguredGroup {
  group: keyof Omit<CapabilityReport, 'mode' | 'workspace' | 'model'>;
  title: string;
  detail: string;
  entries: CapabilityEntry[];
}

const GROUP_ORDER: readonly ConfiguredGroup['group'][] = [
  'plugins',
  'experts',
  'teams',
  'bundles',
  'mcp',
  'skills',
];

const GROUP_TEXT: Record<ConfiguredGroup['group'], { title: string; detail: string }> = {
  plugins: {
    title: '插件',
    detail: '用插件协议通信的子进程；只有已启用的才会被提供',
  },
  experts: {
    title: '专家',
    detail: '除非被禁用，`expert` 工具可以召唤的名册条目',
  },
  teams: {
    title: '团队',
    detail: '并行度大于零时，`team` 工具可以运行的名册团队',
  },
  bundles: {
    title: '市场插件包',
    detail: '从市场安装的；插件包本身就是插件，所以它会故意出现两次',
  },
  mcp: {
    title: 'MCP 服务器',
    detail: '在配置里登记的名字；每一个是否应答过，写在命令行说明里',
  },
  skills: {
    title: '技能',
    detail: '在已配置的根目录下找到的；`skill` 工具会按需读取它们',
  },
};

function text(value: unknown): string {
  return typeof value === 'string' ? value : '';
}

function entries(value: unknown): CapabilityEntry[] {
  if (!Array.isArray(value)) return [];
  const out: CapabilityEntry[] = [];
  for (const raw of value) {
    if (typeof raw !== 'object' || raw === null) continue;
    const row = raw as { name?: unknown; detail?: unknown; active?: unknown };
    const name = text(row.name);
    if (name === '') continue;
    out.push({
      name,
      detail: text(row.detail),
      // Only an explicit true counts. A missing flag reads as "not offered",
      // which is the safe direction for a pane about what the model can reach.
      active: row.active === true,
    });
  }
  return out;
}

/** Decode a response without trusting its shape. */
export function parseReport(value: unknown): CapabilityReport {
  const body = typeof value === 'object' && value !== null ? (value as Record<string, unknown>) : {};
  return {
    mode: text(body.mode),
    workspace: text(body.workspace),
    model: text(body.model),
    plugins: entries(body.plugins),
    experts: entries(body.experts),
    teams: entries(body.teams),
    bundles: entries(body.bundles),
    skills: entries(body.skills),
    mcp: entries(body.mcp),
  };
}

/** Non-empty groups in a fixed order, so the pane does not reshuffle on reload. */
export function configuredGroups(report: CapabilityReport): ConfiguredGroup[] {
  const groups: ConfiguredGroup[] = [];
  for (const group of GROUP_ORDER) {
    const found = report[group];
    if (found.length === 0) continue;
    groups.push({ group, ...GROUP_TEXT[group], entries: found });
  }
  return groups;
}

/** How many rows in the report are configured but not offered right now. */
export function inactiveCount(report: CapabilityReport): number {
  let count = 0;
  for (const group of GROUP_ORDER) {
    for (const entry of report[group]) {
      if (!entry.active) count += 1;
    }
  }
  return count;
}