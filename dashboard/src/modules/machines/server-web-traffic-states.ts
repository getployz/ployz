/** Which way the switch was flipped: on starts the web server on the Server, off stops it. */
export const WEB_TRAFFIC_WANTS = ["on", "off"] as const;
export type WebTrafficWant = (typeof WEB_TRAFFIC_WANTS)[number];

/**
 * One web traffic change Cloud applies to one Server, as its row moves: the flip writes `pending`, the run claims it
 * `running` right before the Engine is asked, and the Engine's answer ends it `finished` or `failed` with why.
 */
export const WEB_TRAFFIC_STATES = ["pending", "running", "finished", "failed"] as const;
export type WebTrafficState = (typeof WEB_TRAFFIC_STATES)[number];
