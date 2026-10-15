/** Names recorded tapes in `golden/` still call, though no current tool has them. */
export const LEGACY_TAPE_TOOLS = {
  set: "T2, T7 and T10 tapes write Settings with it; service_update replaced it.",
  service_rm: "S1 and T15 tapes remove a Service with it; service_remove replaced it.",
} as const satisfies Record<string, string>;

export const TAPE_WRITES = new Set(["set", "service_add", "service_rm", "volume_add", "discard", "domain_add"]);

export const ACTS = new Set([
  "service_restart", "service_start", "service_stop", "server_drain", "server_upgrade", "server_clean", "volume_mirror",
  "volume_mirror_remove", "volume_move", "volume_release", "volume_restore", "environment_remove", "project_remove", "server_remove",
  "server_forget", "server_set",
]);

export const STORE_WRITES = new Set([
  ...TAPE_WRITES, "deploy", "project_create", "service_update", "service_rename", "service_remove", "volume_update",
  "volume_remove", "domain_update", "domain_remove", "config_write", "config_remove", "config_rename", "project_rename", "history",
  "deployment_cancel", "deployment_retry", "deployment_start", "environment_pr",
]);
