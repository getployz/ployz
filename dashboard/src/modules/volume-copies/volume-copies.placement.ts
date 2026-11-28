import type { ChangeKind, MachineId, MachineName, VolumeCopy } from "@ployz/sdk";
import {
  appliedMirrors, detachedName, observePlacement, type PlacementItem, type PlacementRow, type PlacementVolume, placementForm,
} from "#/modules/volume-placement/volume-placement";
import { runWords } from "#/modules/volume-run/run-words";
import { copyName, type Heard, heardFrom, type Member, type VolumeRunView } from "#/modules/volume-run/volume-run";
import type { VolumeRunKind } from "#/modules/volume-run/volume-run-states";
import { HEARD_WORDS, type MirrorCopy, type OldCopy, type PrimaryServer, type StagedCopy, type VolumeCopies } from "./volume-copies";

/** What a run says on the copy it targets: seeding or removing one; the others show on Live or on no copy. */
const BUSY_ON_TARGET = {
  mirror: "Seeding", delete_mirror: "Removing", sync: null, move: null, release: null, restore: null, seed: null, resize: null, deploy: null,
} as const satisfies Record<VolumeRunKind, string | null>;

/** The runs that keep Live from taking another, and so say so on its badge. */
const BUSIES_LIVE = {
  move: true, release: true, restore: true, seed: true, deploy: true, mirror: false, sync: false, delete_mirror: false, resize: false,
} as const satisfies Record<VolumeRunKind, boolean>;

/** What the mirror syncs last saw of one mirror. A pull that lands later never clears an older failure. */
export type MirrorStatusView = {
  readonly machine: MachineId;
  readonly okAt: string | null;
  readonly matchedAt: string | null;
  readonly failedAt: string | null;
  readonly failure: string | null;
};

/**
 * Today's reads in copies words: the Placement form, the Volume's active run and what the mirror syncs saw. The one
 * place that knows placement rows and Volume Runs, so a rewrite of either touches only this file.
 */
export function copiesFromPlacement(input: {
  readonly volume: PlacementVolume;
  readonly members: readonly Member[];
  readonly row: PlacementRow | null;
  readonly active: VolumeRunView | null;
  readonly statuses: readonly MirrorStatusView[];
}): VolumeCopies | null {
  const { volume, members, row, active } = input;
  const form = placementForm(volume, members, row);
  if (form === null) return null;
  const observed = observePlacement(members);
  const moving = form.primary.change !== null && row?.staged?.seed === undefined;
  const promoted = moving && appliedMirrors(observed, row).includes(form.primary.server.id);
  const said = new Map(members.map((member) => [member.machine.id, heardFrom(member)]));
  const heard = (id: MachineId): Heard => said.get(id) ?? "silent";
  const slotOf = (id: MachineId) => {
    const member = members.find((each) => each.machine.id === id);
    return member?.answered === true && member.view.copy?.kind === "slot" ? member.view.copy : null;
  };
  const target = active === null ? null : runTarget(active);

  const busyOn = (server: MachineName): string | null => (active !== null && target === server ? BUSY_ON_TARGET[active.kind] : null);
  const mirror = (item: PlacementItem, staged: StagedCopy | null): MirrorCopy => {
    const status = input.statuses.find((each) => each.machine === item.server.id);
    const matchedAt = status?.matchedAt == null ? null : new Date(status.matchedAt);
    const slot = slotOf(item.server.id);
    const failed = slot?.pull?.failure != null || (status?.failedAt != null && (status.okAt === null || status.failedAt > status.okAt));
    return {
      kind: "mirror",
      id: item.server.id,
      server: item.server,
      name: copyName(volume.name, item.server.name),
      refreshedAt: syncedAt(item.newest, matchedAt),
      failed,
      staged,
      busy: active?.kind === "sync" || (slot !== null && pulling(slot)) ? "Refreshing" : busyOn(item.server.name),
    };
  };
  const old = (item: PlacementItem): OldCopy => ({
    kind: "old",
    id: item.server.id,
    server: item.server,
    name: detachedName(volume),
    since: item.newest,
    bytes: null,
    staged: stagedOf(item.change),
    busy: busyOn(item.server.name),
  });

  const readOnly = (id: MachineId) => members.some((member) =>
    member.answered && member.machine.id === id && member.view.copy?.kind === "root" && member.view.copy.readonly);
  // Live names only the Server that claims the primary: by answering with it, or by its last report while it is unheard.
  const { holder } = observed;
  const primary = ((): PrimaryServer | null => {
    if (holder === null) return null;
    const from = heard(holder.id);
    return { ...holder, state: from !== "answered" ? from : readOnly(holder.id) ? "read_only" : "live" };
  })();
  // A mirror shows only while its Server reports it; an unheard one is named in the unheard line instead.
  const reported = (item: PlacementItem) => heard(item.server.id) === "answered";
  const seed = row?.staged?.seed;
  return {
    volume: { id: volume.id, name: volume.name },
    live: {
      server: primary,
      restore: seed === undefined ? null : { label: seed.point.name },
      moveTo: moving ? form.primary.server : null,
      busy: active !== null && BUSIES_LIVE[active.kind] ? runWords(active) : null,
    },
    elsewhere: [
      ...form.mirrors.filter(reported).map((item) => mirror(item, stagedOf(item.change))),
      ...promoted && reported(form.primary) ? [mirror(form.primary, "primary")] : [],
      ...form.detached.map(old),
    ],
    unheard: members.flatMap((member) => {
      const from = heardFrom(member);
      return from === "answered" || member.machine.id === primary?.id ? [] : [{ ...member.machine, heard: from }];
    }),
  };
}

/**
 * Why Copies can't name the Server that holds the Volume, or null when one claims it or nothing keeps Copies from knowing.
 * Several Servers claiming it is a conflict only a person can settle; with none, an unheard Server may hold it.
 */
export function holderUnknown(volume: PlacementVolume, members: readonly Member[]): string | null {
  const { claims, holder } = observePlacement(members);
  if (holder !== null) return null;
  if (claims.length > 1) {
    return `${claims.map((server) => server.name).join(" and ")} each report holding ${volume.name}, so Copies can't tell which one does or change its copies.`;
  }
  const unheard = members.flatMap((member) => {
    const from = heardFrom(member);
    return from === "answered" ? [] : [HEARD_WORDS[from].clause(member.machine.name)];
  });
  if (unheard.length === 0) return null;
  return `${unheard.join(" and ")}, so Copies can't tell which server holds ${volume.name} or change its copies.`;
}

/** A Machine reports a landed pull until the same pull is asked for again, so a slot pulls only while short of its target. */
function pulling({ pull, newest }: Extract<VolumeCopy, { kind: "slot" }>): boolean {
  return pull !== null && pull.failure === null && newest?.name !== pull.target;
}

/**
 * A sync that finds the mirror already at the writer's newest still means the mirror matches as of that sync. A mirror
 * with no snapshot has never received data, so an older match says nothing about it.
 */
function syncedAt(newest: Date | null, matchedAt: Date | null): Date | null {
  if (newest === null || matchedAt === null) return newest;
  return matchedAt > newest ? matchedAt : newest;
}

function stagedOf(change: ChangeKind | null): StagedCopy | null {
  if (change === "add") return "new";
  if (change === "remove") return "removing";
  return null;
}

function runTarget(run: VolumeRunView): MachineName | null {
  const { args } = run;
  if ("to" in args) return args.to;
  if ("slot" in args) return args.slot;
  return null;
}
