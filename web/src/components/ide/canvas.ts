// Can this environment host a real editor?
//
// Monaco needs a layout engine and a 2D canvas to measure text. jsdom — the
// test environment — implements neither and returns null from `getContext`. A
// capability probe answers that question about the environment; a test flag
// would answer a question about the test runner instead, and the fallback would
// then be exercised nowhere near a browser.
//
// Its own module, imported statically, because the component that asks must not
// pull Monaco in to ask: the answer decides whether Monaco is loaded at all.

/** True when a 2D canvas context can be created here. */
export function canvasAvailable(): boolean {
  if (typeof document === 'undefined') return false;
  const probe = document.createElement('canvas');
  if (typeof probe.getContext !== 'function') return false;
  return probe.getContext('2d') !== null;
}

let cached: boolean | null = null;

/**
 * The same answer, computed once per page.
 *
 * The environment cannot gain a canvas while the page is running, and the probe
 * allocates one each time it is asked — in a host without canvas support that is
 * a warning printed on every mount for a fact that never changes.
 */
export function canvasAvailableOnce(): boolean {
  cached ??= canvasAvailable();
  return cached;
}