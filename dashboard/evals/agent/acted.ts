/** The Store writes T8 counts, each one Batch. Old names stay so recorded tapes still grade. */
export const WRITES = new Set(["set", "service_add", "service_rm", "volume_add", "volume_rm", "discard", "domain_add", "domain_rm", "domain_set"]);

/** The tools that change a Machine or Cloud's view of one. */
export const ACTS = new Set([
  "service_restart", "service_start", "service_stop", "server_drain", "server_upgrade", "server_clean", "volume_mirror",
  "volume_mirror_remove", "volume_move", "volume_release", "environment_remove", "project_remove", "server_remove", "server_forget",
]);

/** The tools that write the Store. */
export const STORE_WRITES = new Set([
  ...WRITES, "deploy", "publish", "config_add", "config_put", "config_mount", "service_add", "project_new", "project_create", "env_new",
  "service_update", "service_rename", "service_remove", "volume_update", "volume_remove", "domain_update", "domain_remove",
  "config_write", "config_remove", "deployment_cancel", "environment_pr",
]);
