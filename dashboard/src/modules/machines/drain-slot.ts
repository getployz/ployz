import { serverDrainRequestedEvent } from "#/modules/inngest/events";

/**
 * One Organization's Drain slot. A Drain takes it, and so does a Server Policy change that turns services on, so that
 * change never applies mid-Drain. Any other policy change keys on its Server and never waits behind a Drain. Inngest
 * shares a slot across functions only when the key expression's text matches, not just its value, so both functions
 * use this one expression. `env` scope shares it across functions; `account` would share it across environments.
 */
export const DRAIN_SLOT = {
  scope: "env",
  key: `event.name == "${serverDrainRequestedEvent}" || event.data.change.acceptsServices == true`
    + ` ? "server-drain-" + event.data.organizationId : "server-policy-" + event.data.machineId`,
  limit: 1,
} as const;
