// The capability panel: what the last request could call, and what this process
// was configured with.
//
// Two sources, on purpose, because they answer different questions:
//
//   * `names` comes from the session's own event log (state/capabilities.ts) and
//     answers "what reached the model in the last request".
//   * `report` comes from `GET /api/capabilities` and answers the question the
//     log cannot: "what is installed but was not offered" — the plugin the
//     operator forgot to enable, the expert the roster marks disabled.
//
// It stayed a view when the endpoint arrived: nothing here starts, stops or
// installs anything, because no route could. The one thing it still cannot see
// is whether an MCP server actually answered, and it says so instead of showing
// a configured server as healthy.
import { configuredGroups, inactiveCount, type CapabilityReport } from '../api/capabilities';
import { sectionsFor, type CapabilitySection } from '../state/capabilities';

export interface CapabilityPanelProps {
  /** Names from the newest `llm/call` event; empty before the first request. */
  names: readonly string[];
  /** Configured set from `GET /api/capabilities`; null until it answers. */
  report?: CapabilityReport | null;
  /** Why the configured set is missing, when it is. */
  reportError?: string | null;
}

function PlatformNote({ sections }: { sections: readonly CapabilitySection[] }) {
  const platform = sections.find((section) => section.group === 'platform');
  if (platform === undefined) {
    // Not an error and not unknown: the names went out in the request, and the
    // two P3 tools simply were not among them.
    return (
      <p className="muted capability-note">
        <code>expert</code> 和 <code>team</code> 都没有出现在这次请求里，
        所以本次会话访问不到专家名册和团队任务板。
      </p>
    );
  }
  return (
    <p className="muted capability-note">
      <code>expert</code> 的用途就是召唤名册里的专家；<code>team</code> 的用途则是维护一块
      任务板。下面列出的已配置条目说明了有哪些专家和团队。
    </p>
  );
}

function Configured({ report, error }: { report: CapabilityReport | null; error: string | null }) {
  if (error !== null) {
    return (
      <p className="muted capability-note">
        无法读取已配置的集合：<code>{error}</code>。本面板只列出上次请求携带的内容；
        <code>hearth plugin list</code>、<code>hearth expert list</code> 和{' '}
        <code>hearth team list</code> 可以从命令行读取它。
      </p>
    );
  }
  if (report === null) {
    return <p className="muted capability-note">正在读取已配置的集合…</p>;
  }
  const groups = configuredGroups(report);
  if (groups.length === 0) {
    return (
      <p className="muted capability-note">
        没有任何已配置但未被使用的项：这个进程在磁盘上没有插件、名册、插件包、MCP 服务器或
        技能。这是一种配置状态，不是故障。
      </p>
    );
  }
  const off = inactiveCount(report);
  return (
    <div className="capability-configured">
      <h3 className="capability-title">
        已配置{' '}
        <span className="muted">
          {off === 0 ? '全部已提供' : `${String(off)} 项未提供`}
        </span>
      </h3>
      {groups.map((group) => (
        <div className="capability-section" key={group.group}>
          <h3 className="capability-title">
            {group.title} <span className="muted">{String(group.entries.length)}</span>
          </h3>
          <p className="muted capability-detail">{group.detail}</p>
          <ul className="capability-tools">
            {group.entries.map((entry) => (
              <li
                className={
                  entry.active
                    ? 'mono capability-tool'
                    : 'mono capability-tool capability-tool-off'
                }
                key={`${group.group}:${entry.name}`}
                title={entry.detail}
              >
                {entry.name}
                {entry.active ? null : <span className="capability-off">（未提供）</span>}
              </li>
            ))}
          </ul>
        </div>
      ))}
    </div>
  );
}

export function CapabilityPanel({ names, report = null, reportError = null }: CapabilityPanelProps) {
  const sections: CapabilitySection[] = sectionsFor(names);
  const total = sections.reduce((count, section) => count + section.tools.length, 0);

  if (total === 0) {
    return (
      <section className="capabilities" aria-label="能力">
        <h2 className="capabilities-head">工具</h2>
        <p className="muted capabilities-empty">
          本次会话还没有 <code>llm/call</code> 事件，因此那条会列出工具的请求尚未写入。
          这里不做任何猜测：空列表意味着“还没有发起请求”，而不是“没有安装工具”。
        </p>
        <Configured report={report} error={reportError} />
      </section>
    );
  }

  return (
    <section className="capabilities" aria-label="能力">
      <h2 className="capabilities-head">
        工具 <span className="muted">上次请求共 {String(total)} 个</span>
      </h2>
      {sections.map((section) => (
        <div className="capability-section" key={section.group}>
          <h3 className="capability-title">
            {section.title} <span className="muted">{String(section.tools.length)}</span>
          </h3>
          <p className="muted capability-detail">{section.detail}</p>
          <ul className="capability-tools">
            {section.tools.map((tool) => (
              <li className="mono capability-tool" key={tool.name} title={tool.group}>
                {tool.name}
              </li>
            ))}
          </ul>
        </div>
      ))}
      <PlatformNote sections={sections} />
      <Configured report={report} error={reportError} />
    </section>
  );
}