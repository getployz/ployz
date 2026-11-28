import type { ContainerId, MachineId, MachineName, Namespace, ResolvedServiceSpec, SlotId, SlotRole, SnapshotGuid, VolumeCopyView } from "@ployz/sdk";
import { Schema } from "effect";
import type { VolumeRunState, VolumeRunRequestKind, VolumeRunKind } from "#/modules/volume-run/volume-run-states";

export type MachineRef = { readonly id: MachineId; readonly name: MachineName };

/** A Volume of an Organization, as every volume server function names it. */
export const VolumeRef = Schema.Struct({ organizationSlug: Schema.NonEmptyString, volumeId: Schema.NonEmptyString });

/** A Volume of an Environment: what the server functions that read or change its copies take. */
export const VolumeInput = Schema.Struct({
  ...VolumeRef.fields,
  environment: Schema.Struct({ project: Schema.NullOr(Schema.String), environment: Schema.NullOr(Schema.String) }),
});

export const ACTIVE_VOLUME_RUN_STATES = ["requested", "running"] as const satisfies readonly VolumeRunState[];

/** What a run of each kind is called when a Server can't run it. */
export const RUN_FEATURES = {
  mirror: "Volume mirror", sync: "Mirror refresh", delete_mirror: "Mirror removal", move: "Volume move", release: "Volume move", restore: "Using a mirror", seed: "Volume restore",
  resize: "Volume resize", deploy: "Deploy",
} satisfies Record<VolumeRunKind, string>;

/** The sidebar also pins the Machine it resolved, so a renamed or reused Server name can't redirect the run. */
type Destination = { readonly to: MachineName; readonly machine?: MachineId };

/**
 * What a run's Servers confirmed it did, recorded as each answers: the Containers a Server acknowledged removing, and
 * the Server a Release started the Service on again. A row from before runs recorded it has none.
 */
export type VolumeRunResult = { readonly removed?: readonly ContainerId[]; readonly restarted?: MachineId };

/**
 * What a requested Release's plan showed: the slots it may resume from and the slots it may forget. The run refuses to
 * resume or forget anywhere else, so it never deletes a copy the plan did not name. Cloud's own Release of a Move
 * it ended carries none. `forward` pins a release that finishes a committed Move: the commitment it follows, the
 * Server it starts on and the Servers it removes the moved-off Container from.
 */
export type ReleasePlan = {
  readonly sources: MachineId[];
  readonly forgets: MachineId[];
  readonly forward?: { readonly after: string; readonly to: MachineId; readonly removeOn: MachineId[] };
};

/**
 * A copy someone chose to delete, as its Server reported it when they did. `written_here` is null on a Server that
 * doesn't say whether the copy may hold writes no other Server has.
 */
export type ConfirmedCopy = { readonly machine: MachineId; readonly slot: SlotId; readonly newest: SnapshotGuid; readonly written_here: boolean | null };

/** Who chose a delete, and each copy it deletes as they saw it: a person who asked, or a Deployment they confirmed. */
export type Confirmation = { readonly by: "person" | "deployment"; readonly copies: ConfirmedCopy[] };

declare const chosen: unique symbol;

/**
 * A Confirmation as a run's row keeps it. Only the insert of the row makes one, from its caller, so a delete no one chose,
 * such as the orphan sweep's, has none. A Forget says discard only for a copy listed here, and names that exact copy, so
 * its Server deletes nothing replaced or written since. The brand sits on `by`, a string, so it survives a step's JSON.
 */
export type Discard = { readonly by: Confirmation["by"] & { readonly [chosen]: true }; readonly copies: ConfirmedCopy[] };

export type VolumeRunArgs = {
  mirror: Destination;
  sync: { readonly full: boolean };
  /**
   * `role` is what the request was admitted to delete: an old copy, or a mirror a Deploy removes. The run's Forget names it,
   * so the Machine refuses a slot that holds the other role by then. `any` is `volume mirror rm`, which deletes either. A
   * row without one was admitted before runs recorded it, and is refused rather than guessed.
   */
  delete_mirror: {
    readonly slot: MachineName | null;
    readonly confirmed_name: string | null;
    readonly machine?: MachineId;
    readonly role?: SlotRole | "any";
    readonly discard?: Discard;
  };
  /**
   * `holder` is a drain's: the Server it drains and the Container it saw run the Service there. The move takes only that
   * Container, so one replaced or stopped before the cut ends the move before anything stops.
   */
  move: Destination & { readonly holder?: { readonly machine: MachineId; readonly container: ContainerId } };
  /**
   * `follows` marks a Deploy's release: it is admitted only while the Volume still finishes on that Machine.
   */
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
   * A Deploy's own lane while its Services run; `resume` is the move it took over, requested again once it ends. `hold` marks a lane on a Volume no run held: it
   * took nothing, so it is never fenced and leaves no row when the Deploy ends.
   */
  deploy: {
    readonly deployment: string;
    readonly resume: Destination | null;
    readonly hold?: true;
  };
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
      /** The Server checks where a copy's data began, so it may send or take a born Volume's copy. */
      readonly checksOrigins?: boolean;
      /** The Server keeps a slot that may hold writes no other Server has: a bare Forget refuses it, and a pull never replaces it. */
      readonly keepsWritten?: boolean;
    }
  | {
      readonly machine: { readonly id: MachineId; readonly name: MachineName };
      readonly answered: false;
      readonly lack?: string;
      /** The lack is the Server's release being behind its line's latest, so an upgrade brings what it lacks. */
      readonly outdated?: true;
      /** The frame lists the Machine only by id: its report hasn't arrived, which is not the same as not answering. */
      readonly incomplete?: true;
      /**
       * The Runtime Watch's last record of this Server's copy is a root: the Server's own last report that it holds the
       * primary. The Cluster keeps a silent Server's rows until it answers again or is forgotten.
       */
      readonly lastRoot?: true;
      /**
       * A resize's Server that reports no managed storage, so it wasn't asked. Every plan leaves it out but the record of
       * a Volume no Server holds, since a Server whose `zpool` is missing while its pool is imported reports so too.
       */
      readonly stateless?: true;
    };
export type AnsweredMember = Extract<Member, { answered: true }>;

type RunServer = { readonly id: MachineId; readonly name: MachineName | null };

/** Whether a run's args name this Server: by its machine id, or by its name in rows from before runs recorded the id. */
export const namesServer = (args: Destination | VolumeRunArgs["restore"], server: RunServer) =>
  args.machine === undefined ? server.name === ("to" in args ? args.to : args.from) : server.id === args.machine;

export const isDestination = (run: VolumeLaneInput, machine: RunServer) =>
  (run.kind === "move" || run.kind === "mirror") && namesServer(run.args, machine);

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

export const copyName = (volumeName: string, server: string) => `${volumeName}-${server}`;

/** A Service that mounts the Volume, as a Service container reports it with its secrets redacted. */
export type ServiceSpec = { readonly namespace: Namespace; readonly redactedSpec: ResolvedServiceSpec };

export const STALE_AFTER_MS = 60 * 60_000;
