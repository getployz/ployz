import { builtinEnvironments } from "vitest/environments";

/**
 * jsdom for a postgres test that renders the dashboard. Import it first: its server code needs the node environment,
 * since the `jsdom` one transforms it for the browser, and React reads `window` as it loads.
 */
const { teardown } = await builtinEnvironments.jsdom.setup(globalThis, {});

export const teardownJsdom = () => teardown(globalThis);
