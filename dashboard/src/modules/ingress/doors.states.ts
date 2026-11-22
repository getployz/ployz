/** Which Door the Cluster ends up behind: its public ports, or the Cloudflare tunnel. */
export const DOORS_WANTS = ["tunnel", "public"] as const;
export type DoorsWant = (typeof DOORS_WANTS)[number];

export const DOORS_STATES = ["pending", "running", "finished", "failed"] as const;
export type DoorsState = (typeof DOORS_STATES)[number];

/**
 * Where a switch is, for the progress the page shows: both Doors open on every Server, generated names moving to the
 * other source, then the old Door closing.
 */
export const DOORS_STAGES = ["opening", "moving", "closing"] as const;
export type DoorsStage = (typeof DOORS_STAGES)[number];

export const CLOUDFLARE_SETUP_STATES = ["setting_up", "set_up", "active", "left"] as const;
export type CloudflareSetupState = (typeof CLOUDFLARE_SETUP_STATES)[number];
