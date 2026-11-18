// The capability panel: what the last request could call, grouped by kind.
//
// The source is the session's own event log (see state/capabilities.ts), so this
// panel adds no endpoint and no request. It answers the question an operator
// actually asks after wiring in a plugin or a roster — "did that reach the
// model?" — without leaving the browser to guess at configuration it cannot see.
//
// It is a view, not a control surface: nothing here starts, stops or installs
// anything, because the HTTP API has no route that could. The one thing it
// cannot answer — which plugins are installed but were not offered — is said on
// screen rather than left to inference, so nobody reads an empty section as
// "the harness has no plugins".
import { sectionsFor, type CapabilitySection } from '../state/capabilities';

export interface CapabilityPanelProps {
  /** Names from the newest `llm/call` event; empty before the first request. */
  names: readonly string[];
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
      board. Which experts exist and which board ran is decided by the CLI and the data
      directory — this pane cannot list either, because the server publishes no route for it.
    </p>
  );
}

export function CapabilityPanel({ names }: CapabilityPanelProps) {
  const sections: CapabilitySection[] = sectionsFor(names);
  const total = sections.reduce((count, section) => count + section.tools.length, 0);

  if (total === 0) {
    return (
      <section className="capabilities" aria-label="tools in the last request">
        <h2 className="capabilities-head">tools</h2>
        <p className="muted capabilities-empty">
          this session has no <code>llm/call</code> event yet, so the request that would name
          the tools has not been written. Nothing is guessed here: an empty pane means “not
          requested yet”, not “no tools installed”.
        </p>
      </section>
    );
  }

  return (
    <section className="capabilities" aria-label="tools in the last request">
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
      <p className="muted capability-note">
        installed plugins and expert rosters that were <em>not</em> offered here cannot be read
        from the browser: there is no endpoint for them. use <code>forge plugin list</code>,
        <code>forge expert list</code> and <code>forge team list</code> for the configured
        set, and this pane for what actually reached the model.
      </p>
    </section>
  );
}
