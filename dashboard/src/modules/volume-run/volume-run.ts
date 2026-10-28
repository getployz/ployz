import type { MachineId, MachineName, Namespace, ResolvedServiceSpec, SlotRole, VolumeCopyView } from "@ployz/sdk";

export const VOLUME_RUN_STATES = ["requested", "running", "done", "failed", "cancelled", "lost", "not_started", "yielded"] as const;
export type VolumeRunState = (typeof VOLUME_RUN_STATES)[number];
export const VOLUME_RUN_PHASES = ["warm", "committed"] as const;
export type VolumeRunPhase = (typeof VOLUME_RUN_PHASES)[number];

export const ACTIVE_VOLUME_RUN_STATES = ["requested", "running"] as const satisfies readonly VolumeRunState[];

/** The kinds a caller requests. A `deploy` row is never requested; a Deploy inserts it as its own lane. */
export const VOLUME_RUN_REQUEST_KINDS = ["mirror", "sync", "delete_mirror", "move", "release", "restore", "seed", "resize"] as const;
export type VolumeRunRequestKind = (typeof VOLUME_RUN_REQUEST_KINDS)[number];
export const VOLUME_RUN_KINDS = [...VOLUME_RUN_REQUEST_KINDS, "deploy"] as const;
export type VolumeRunKind = (typeof VOLUME_RUN_KINDS)[number];

/** What a run of each kind is called when a Server can't run it. */
export const RUN_FEATURES = {
  mirror: "Volume mirror", sync: "Mirror refresh", delete_mirror: "Mirror removal", move: "Volume move", release: "Volume move", restore: "Using a mirror", seed: "Volume restore",
  resize: "Volume resize", deploy: "Deploy",
} satisfies Record<VolumeRunKind, string>;

/** The sidebar also pins the Machine it resolved, so a renamed or reused Server name can't redirect the run. */
type Destination = { readonly to: MachineName; readonly machine?: MachineId };

/**
 * What a requested Release's plan showed: the slots it may resume from and the slots it may forget. The run refuses to
 * resume or forget anywhere else, so it never deletes a copy the approval did not name. Cloud's own Release of a Move
 * it ended carries none. `forward` pins a release that finishes a committed Move: the commitment it follows, the
 * Server it starts on and the Servers it removes the moved-off Container from.
 */
export type ReleasePlan = {
  readonly sources: MachineId[];
  readonly forgets: MachineId[];
  readonly forward?: { readonly after: string; readonly to: MachineId; readonly removeOn: MachineId[] };
};

export type VolumeRunArgs = {
  mirror: Destination;
  sync: { readonly full: boolean };
  /**
   * `role` is what the request was admitted to delete: an old copy, or a mirror a Deploy removes. The run's Forget names it,
   * so the Machine refuses a slot that holds the other role by then. `any` is `volume mirror rm`, which deletes either. A
   * row without one was admitted before runs recorded it, and is refused rather than guessed.
   */
  delete_mirror: { readonly slot: MachineName | null; readonly confirmed_name: string | null; readonly machine?: MachineId; readonly role?: SlotRole | "any" };
  move: Destination;
  /** `follows` marks a Deploy's release: it is admitted only while the Volume still finishes on that Machine. */
  release: { readonly plan?: ReleasePlan; readonly follows?: MachineId };
  restore: { readonly from: MachineName; readonly machine?: MachineId };
  seed: {
    readonly into: { readonly volumeId: string; readonly dockerVolume: string; readonly name: string };
    readonly guid: string;
    readonly name: string;
  };
  /**
   * The size every copy is asked to hold, in whole bytes, as `SetVolumeSizeRequest` names it. The size it moves from
   * is the row's `recordedBytes`, read from the Store when the run was requested.
   */
  resize: { readonly maximum_bytes: number };
  /**
   * A Deploy's own lane while its Services run; `resume` is the move it took over, requested again once it ends. `hold`
   * marks a lane on a Volume no run held: it took nothing, so it is never fenced and leaves no row when the Deploy ends.
   */
  deploy: { readonly deployment: string; readonly resume: Destination | null; readonly hold?: true };
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
      /** A Server without the generation floor; it holds no copy and is not the run's destination. */
      readonly floorless?: boolean;
      /** For a resize, the copy's bound in the Runtime Watch frame; null when the frame shows no sized copy here. */
      readonly boundBytes?: number | null;
    }
  | {
      readonly machine: { readonly id: MachineId; readonly name: MachineName };
      readonly answered: false;
      readonly lack?: string;
      /** The lack is the Server's release being behind its line's latest, so an upgrade brings what it lacks. */
      readonly outdated?: true;
      /** The frame lists the Machine only by id: its report hasn't arrived, which is not the same as not answering. */
      readonly incomplete?: true;
      /** The Runtime Watch's last record of this Server's copy is a mirror. Copies shows it; staging and recovery never read it. */
      readonly lastMirror?: true;
      /**
       * A resize's Server that reports no managed storage, so it wasn't asked. Every plan leaves it out but the record of
       * a Volume no Server holds, since a Server whose `zpool` is missing while its pool is imported reports so too.
       */
      readonly stateless?: true;
    };
export type AnsweredMember = Extract<Member, { answered: true }>;

type RunServer = { readonly id: MachineId; readonly name: MachineName | null };

export const isDestination = (run: VolumeLaneInput, machine: RunServer) =>
  (run.kind === "move" || run.kind === "mirror") && (run.args.machine === undefined ? run.args.to === machine.name : run.args.machine === machine.id);

/** Whether a run needs the generation floor on a Server: one that holds a copy, or the run's destination. */
export const needsFloor = (run: VolumeLaneInput, machine: RunServer, copy: VolumeCopyView["copy"]) => copy !== null || isDestination(run, machine);

/**
 * What this observer heard from a member's Server just now. Only `answered` reports the copy. A Server that `lacks` the
 * capability answered without reporting it, and is `outdated` when its release is behind. One the frame lists only by
 * id is `waiting`: its report hasn't arrived, which says nothing about whether it answers.
 */
export type Heard = "answered" | "outdated" | "lacks" | "silent" | "waiting";

export function heardFrom(member: Member): Heard {
  if (member.answered) return "answered";
  if (member.lack !== undefined) return member.outdated ? "outdated" : "lacks";
  return member.incomplete ? "waiting" : "silent";
}

/**
 * The members that didn't report their copy, by why: what each lacking Server lacks, silent Servers' names, and whether
 * a report is still on its way. A lack comes first, as the planner refuses: waiting never clears it.
 */
export function unheardOf(members: readonly Member[]) {
  const lacks: string[] = [];
  const silent: string[] = [];
  let waiting = false;
  for (const member of members) {
    if (member.answered) continue;
    const heard = heardFrom(member);
    if (heard === "silent") silent.push(member.machine.name);
    else if (heard === "waiting") waiting = true;
    else if ((heard === "lacks" || heard === "outdated") && member.lack !== undefined) lacks.push(member.lack);
  }
  return { lacks, silent, waiting };
}

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
