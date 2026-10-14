import type { MachineId, MachineName, Namespace, ResolvedServiceSpec, VolumeCopyView } from "@ployz/sdk";

export const VOLUME_RUN_STATES = ["requested", "running", "done", "failed", "cancelled", "lost", "not_started", "yielded"] as const;
export type VolumeRunState = (typeof VOLUME_RUN_STATES)[number];
export const VOLUME_RUN_PHASES = ["warm", "committed"] as const;
export type VolumeRunPhase = (typeof VOLUME_RUN_PHASES)[number];

export const ACTIVE_VOLUME_RUN_STATES = ["requested", "running"] as const satisfies readonly VolumeRunState[];

/** The kinds a caller requests. A `deploy` row is never requested; a Deploy inserts it as its own lane. */
export const VOLUME_RUN_REQUEST_KINDS = ["mirror", "sync", "delete_mirror", "move", "release", "restore", "seed"] as const;
export type VolumeRunRequestKind = (typeof VOLUME_RUN_REQUEST_KINDS)[number];
export const VOLUME_RUN_KINDS = [...VOLUME_RUN_REQUEST_KINDS, "deploy"] as const;
export type VolumeRunKind = (typeof VOLUME_RUN_KINDS)[number];

/** The sidebar also pins the Machine it resolved, so a renamed or reused Server name can't redirect the run. */
type Destination = { readonly to: MachineName; readonly machine?: MachineId };

/**
 * What a requested Release's plan showed: the slots it may resume from and the slots it may forget. The run refuses to
 * resume or forget anywhere else, so it never deletes a copy the approval did not name. Cloud's own Release of a Move
 * it ended carries none.
 */
export type ReleasePlan = { readonly sources: MachineId[]; readonly forgets: MachineId[] };

export type VolumeRunArgs = {
  mirror: Destination;
  sync: { readonly full: boolean };
  delete_mirror: { readonly slot: MachineName | null; readonly confirmed_name: string | null };
  move: Destination;
  release: { readonly plan?: ReleasePlan };
  restore: { readonly from: MachineName };
  seed: {
    readonly into: { readonly volumeId: string; readonly dockerVolume: string; readonly name: string };
    readonly guid: string;
    readonly name: string;
  };
  /** A Deploy's own lane while its Services run; `resume` is the move it took over, requested again once it ends. */
  deploy: { readonly deployment: string; readonly resume: Destination | null };
};
export type AnyVolumeRunArgs = VolumeRunArgs[VolumeRunKind];

export type VolumeRunInput = {
  [K in VolumeRunRequestKind]: { readonly kind: K; readonly args: VolumeRunArgs[K] }
}[VolumeRunRequestKind];
export type VolumeLaneInput = VolumeRunInput | { readonly kind: "deploy"; readonly args: VolumeRunArgs["deploy"] };

export const VOLUME_RUN_UNCLAIMED_LIMIT_MS = 10 * 60_000;
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
  readonly epoch: number | null;
  readonly message: string | null;
  readonly created_at: string;
  readonly updated_at: string;
  readonly finished_at: string | null;
};

export type Member =
  | {
      readonly machine: { readonly id: MachineId; readonly name: MachineName };
      readonly address: string;
      readonly answered: true;
      readonly view: VolumeCopyView;
    }
  | {
      readonly machine: { readonly id: MachineId; readonly name: MachineName };
      readonly answered: false;
      readonly lack?: string;
    };
export type AnsweredMember = Extract<Member, { answered: true }>;

export function managementAddress(publicKey: readonly number[]): string {
  const bytes = [0xfd, 0xcc, ...publicKey.slice(0, 14)];
  if (bytes.length !== 16) throw new Error("A WireGuard public key has 32 bytes.");
  const groups: string[] = [];
  for (let index = 0; index < 16; index += 2) {
    groups.push((((bytes[index] ?? 0) << 8) | (bytes[index + 1] ?? 0)).toString(16));
  }
  return groups.join(":");
}

export const copyName = (volumeName: string, server: string) => `${volumeName}-${server}`;

export const dockerVolumeName = (namespace: string, volumeId: string) => `${namespace}_vol-${volumeId}`;

export function parseDockerVolumeName(name: string): { namespace: string; volumeId: string } | null {
  const at = name.lastIndexOf("_vol-");
  if (at <= 0) return null;
  const volumeId = name.slice(at + "_vol-".length);
  return volumeId.length === 0 ? null : { namespace: name.slice(0, at), volumeId };
}

/** A Service that mounts the Volume, as a Service container reports it with its secrets redacted. */
export type ServiceSpec = { readonly namespace: Namespace; readonly redactedSpec: ResolvedServiceSpec };
