import type { MachineId, MachineName, VolumeCopyView } from "@ployz/sdk";

export const VOLUME_RUN_STATES = ["requested", "running", "done", "failed", "cancelled", "lost", "not_started"] as const;
export type VolumeRunState = (typeof VOLUME_RUN_STATES)[number];
export const ACTIVE_VOLUME_RUN_STATES = ["requested", "running"] as const satisfies readonly VolumeRunState[];

export const VOLUME_RUN_KINDS = ["mirror", "sync", "delete_mirror"] as const;
export type VolumeRunKind = (typeof VOLUME_RUN_KINDS)[number];

/** What each kind was asked to do. A null `slot` deletes every slot: only orphan runs do. */
export type VolumeRunArgs = {
  mirror: { readonly to: MachineName };
  sync: { readonly full: boolean };
  delete_mirror: { readonly slot: MachineName | null; readonly confirmed_name: string | null };
};
export type AnyVolumeRunArgs = VolumeRunArgs[VolumeRunKind];

/** A request no run claimed in this long never starts. */
export const VOLUME_RUN_UNCLAIMED_LIMIT_MS = 10 * 60_000;
/** A run this long in `running` gets its Inngest run looked up. */
export const VOLUME_RUN_RUNNING_CHECK_MS = 15 * 60_000;
export const VOLUME_RUN_MESSAGE_LIMIT = 1024;

export type VolumeRunView = {
  readonly id: string;
  readonly volume_id: string;
  readonly volume_name: string;
  readonly kind: VolumeRunKind;
  readonly args: AnyVolumeRunArgs;
  readonly orphan: boolean;
  readonly state: VolumeRunState;
  readonly lease: number | null;
  readonly message: string | null;
  readonly created_at: string;
  readonly updated_at: string;
  readonly finished_at: string | null;
};

/**
 * One runtime-frame Machine as 01-observe saw it. `address` is its management address, what StartReceive pulls from.
 * `pool` says it has a Machine Pool: the Volume Switch verbs that write need one, so a Machine without one is never
 * sent AdoptLease and can't take a Mirror.
 */
export type Member =
  | {
      readonly machine: { readonly id: MachineId; readonly name: MachineName };
      readonly address: string;
      readonly answered: true;
      readonly pool: boolean;
      readonly view: VolumeCopyView;
    }
  | { readonly machine: { readonly id: MachineId; readonly name: MachineName }; readonly answered: false };
export type AnsweredMember = Extract<Member, { answered: true }>;

/** The management address of a Machine: fdcc followed by the first 14 bytes of its WireGuard public key. */
export function managementAddress(publicKey: readonly number[]): string {
  const bytes = [0xfd, 0xcc, ...publicKey.slice(0, 14)];
  if (bytes.length !== 16) throw new Error("A WireGuard public key has 32 bytes.");
  const groups: string[] = [];
  for (let index = 0; index < 16; index += 2) {
    groups.push((((bytes[index] ?? 0) << 8) | (bytes[index + 1] ?? 0)).toString(16));
  }
  return groups.join(":");
}

/** How users name one copy: the Volume's name and the Server holding it. */
export const copyName = (volumeName: string, server: string) => `${volumeName}-${server}`;

/** The Docker Volume a config entry deploys as, the name every Machine verb takes. */
export const dockerVolumeName = (namespace: string, volumeId: string) => `${namespace}_vol-${volumeId}`;

/** A Docker Volume name split back into its Namespace and VolumeId, or null for one no config entry could deploy. */
export function parseDockerVolumeName(name: string): { namespace: string; volumeId: string } | null {
  const at = name.lastIndexOf("_vol-");
  if (at <= 0) return null;
  const volumeId = name.slice(at + "_vol-".length);
  return volumeId.length === 0 ? null : { namespace: name.slice(0, at), volumeId };
}
