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
        neither <code>expert</code> nor <code>team</code> was offered in this request, so the
        expert roster and the team board are not reachable from this session.
      </p>
    );
  }
  return (
    <p className="muted capability-note">
      <code>expert</code> summoning is what the roster is for; <code>team</code> is what keeps a
      board. The configured rows below say which experts and teams exist.
    </p>
  );
}

function Configured({ report, error }: { report: CapabilityReport | null; error: string | null }) {
  if (error !== null) {
    return (
      <p className="muted capability-note">
        the configured set could not be read: <code>{error}</code>. this pane lists only what the
        last request carried; <code>hearth plugin list</code>, <code>hearth expert list</code> and{' '}
        <code>hearth team list</code> read it from the CLI.
      </p>
    );
  }
  if (report === null) {
    return <p className="muted capability-note">reading the configured set…</p>;
  }
  const groups = configuredGroups(report);
  if (groups.length === 0) {
    return (
      <p className="muted capability-note">
        nothing is configured but unused: this process has no plugins, rosters, bundles, MCP
        servers or skills on disk. That is a configuration, not a failure.
      </p>
    );
  }
  const off = inactiveCount(report);
  return (
    <div className="capability-configured">
      <h3 className="capability-title">
        configured{' '}
        <span className="muted">
          {off === 0 ? 'all offered' : `${String(off)} not offered`}
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
                {entry.active ? null : <span className="capability-off"> (not offered)</span>}
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
      <section className="capabilities" aria-label="capabilities">
        <h2 className="capabilities-head">tools</h2>
        <p className="muted capabilities-empty">
          this session has no <code>llm/call</code> event yet, so the request that would name
          the tools has not been written. Nothing is guessed here: an empty list means “not
          requested yet”, not “no tools installed”.
        </p>
        <Configured report={report} error={reportError} />
      </section>
    );
  }

  return (
    <section className="capabilities" aria-label="capabilities">
      <h2 className="capabilities-head">
        tools <span className="muted">{String(total)} in the last request</span>
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