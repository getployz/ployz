import type { Permission } from "#/modules/organization/permissions";
import type { VolumeRunRequestKind } from "#/modules/volume-run/volume-run-states";

/** The tool name a request key is scoped to: the same key under another tool is another request. */
export const OPERATION_TOOLS = [
  "server_drain",
  "server_set",
  "server_upgrade",
  "server_clean",
  "server_remove",
  "server_forget",
  "volume_mirror",
  "volume_move",
  "volume_release",
  "volume_mirror_remove",
  "volume_restore",
  "volume_seed",
  "volume_resize",
  "service_restart",
  "service_start",
  "service_stop",
] as const;
export type OperationTool = (typeof OPERATION_TOOLS)[number];

type Idempotency = OperationTool | { readonly none: string };

/** What the Caller must hold on every surface, and the tool a request key is scoped to. */
const operation = <const I extends Idempotency>(requires: Permission, idempotency: I) => ({ requires, idempotency });

/** Every Cloud operation the gate and the key table know. Its CLI, agent and dashboard carriage is in `operations.parity.ts`. */
export const OPERATIONS = {
  "server.remove": operation("servers:remove", "server_remove"),
  "server.forget": operation("servers:remove", "server_forget"),
  "server.drain": operation("servers:operate", "server_drain"),
  "server.upgrade": operation("servers:operate", "server_upgrade"),
  "server.set": operation("servers:operate", "server_set"),
  "namespace.clean": operation("servers:operate", "server_clean"),
  "volume.mirror": operation("services:control", "volume_mirror"),
  "volume.sync": operation("services:control", "volume_mirror"),
  "volume.move": operation("services:control", "volume_move"),
  "volume.release": operation("services:control", "volume_release"),
  "volume.remove_mirror": operation("volumes:delete-copy", "volume_mirror_remove"),
  "volume.restore": operation("services:control", "volume_restore"),
  "volume.resize": operation("services:control", "volume_resize"),
  "volume.seed": operation("changes:stage", "volume_seed"),
  "service.restart": operation("services:control", "service_restart"),
  "service.start": operation("services:control", "service_start"),
  "service.stop": operation("services:control", "service_stop"),
  "cluster.rename": operation("servers:operate", { none: "Saving a name again sets the same name." }),
  "ingress.use": operation("servers:operate", { none: "A switch to where the Cluster already is is refused and changes nothing." }),
  "private_access.use": operation("servers:operate", { none: "Turning it on or off again leaves every Server's router as it is." }),
  "servers.forget_all": operation("servers:remove", { none: "It retries every Server and forgets only when none answers, so a second call finds nothing." }),
} as const;


/** The operation each Volume Run kind a caller requests is. */
export const VOLUME_OPERATIONS = {
  mirror: OPERATIONS["volume.mirror"],
  sync: OPERATIONS["volume.sync"],
  move: OPERATIONS["volume.move"],
  release: OPERATIONS["volume.release"],
  delete_mirror: OPERATIONS["volume.remove_mirror"],
  restore: OPERATIONS["volume.restore"],
  seed: OPERATIONS["volume.seed"],
  resize: OPERATIONS["volume.resize"],
} as const satisfies Record<VolumeRunRequestKind, { readonly idempotency: OperationTool; readonly requires: Permission }>;
