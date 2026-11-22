export const VOLUME_RUN_STATES = ["requested", "running", "done", "failed", "cancelled", "lost", "not_started", "yielded"] as const;
export type VolumeRunState = (typeof VOLUME_RUN_STATES)[number];
export const VOLUME_RUN_PHASES = ["warm", "committed"] as const;
export type VolumeRunPhase = (typeof VOLUME_RUN_PHASES)[number];

/** The kinds a caller requests. A `deploy` row is never requested; a Deploy inserts it as its own lane. */
export const VOLUME_RUN_REQUEST_KINDS = ["mirror", "sync", "delete_mirror", "move", "release", "restore", "seed", "resize"] as const;
export type VolumeRunRequestKind = (typeof VOLUME_RUN_REQUEST_KINDS)[number];
export const VOLUME_RUN_KINDS = [...VOLUME_RUN_REQUEST_KINDS, "deploy"] as const;
export type VolumeRunKind = (typeof VOLUME_RUN_KINDS)[number];
