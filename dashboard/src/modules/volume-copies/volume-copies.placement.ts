import type { ChangeKind, MachineId, MachineName } from "@ployz/sdk";
import {
  appliedMirrors, detachedName, observePlacement, type PlacementItem, type PlacementRow, type PlacementVolume, placementForm,
} from "#/modules/volume-placement/volume-placement";
import { runWords } from "#/modules/volume-run/volume-copies";
import { copyName, type Member, type VolumeRunView } from "#/modules/volume-run/volume-run";
import type { MirrorCopy, OldCopy, StagedCopy, VolumeCopies } from "./volume-copies";

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
  const silent = new Set(members.filter((member) => !member.answered).map((member) => member.machine.id));
  const pullOf = (id: MachineId) => {
    const member = members.find((each) => each.machine.id === id);
    return member?.answered === true && member.view.copy?.kind === "slot" ? member.view.copy.pull : null;
  };
  const target = active === null ? null : runTarget(active);

  const busyOn = (server: MachineName): string | null => {
    if (active === null) return null;
    if (active.kind === "mirror" && target === server) return "Seeding";
    if (active.kind === "delete_mirror" && target === server) return "Removing";
    return null;
  };
  const mirror = (item: PlacementItem, staged: StagedCopy | null): MirrorCopy => {
    const quiet = silent.has(item.server.id);
    const status = input.statuses.find((each) => each.machine === item.server.id);
    const matchedAt = status?.matchedAt == null ? null : new Date(status.matchedAt);
    const pull = pullOf(item.server.id);
    const failed = pull?.failure != null || (status?.failedAt != null && (status.okAt === null || status.failedAt > status.okAt));
    return {
      kind: "mirror",
      id: item.server.id,
      server: item.server,
      name: copyName(volume.name, item.server.name),
      refreshedAt: quiet ? matchedAt : syncedAt(item.newest, matchedAt),
      skipped: quiet ? "silent" : failed ? "failed" : null,
      staged,
      busy: active?.kind === "sync" || (pull !== null && pull.failure === null) ? "Refreshing" : busyOn(item.server.name),
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

  const seed = row?.staged?.seed;
  return {
    volume: { id: volume.id, name: volume.name },
    live: {
      server: observed.primary ?? (form.primary.change === null ? form.primary.server : null),
      restore: seed === undefined ? null : { label: seed.point.name },
      moveTo: moving ? form.primary.server : null,
      busy: active !== null && ["move", "release", "restore", "seed"].includes(active.kind) ? runWords(active) : null,
      snapshots: null,
    },
    elsewhere: [
      ...form.mirrors.map((item) => mirror(item, stagedOf(item.change))),
      ...promoted ? [mirror(form.primary, "primary")] : [],
      ...form.detached.map(old),
    ],
  };
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
