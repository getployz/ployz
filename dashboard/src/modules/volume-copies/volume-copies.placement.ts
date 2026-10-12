import type { ChangeKind, MachineName } from "@ployz/sdk";
import {
  appliedMirrors, detachedName, observePlacement, type PlacementItem, type PlacementRow, type PlacementVolume, placementForm,
  restoreLabel,
} from "#/modules/volume-placement/volume-placement";
import { runWords } from "#/modules/volume-run/volume-copies";
import { copyName, type Member, type VolumeRunView } from "#/modules/volume-run/volume-run";
import type { MirrorCopy, OffsiteCopy, OldCopy, Snapshot, StagedCopy, VolumeCopies } from "./volume-copies";

/**
 * Today's reads in copies words: the Placement form, the Volume's active run and its last mirror refresh. The one place
 * that knows placement rows and Volume Runs, so a rewrite of either touches only this file.
 */
export function copiesFromPlacement(input: {
  readonly volume: PlacementVolume;
  readonly members: readonly Member[];
  readonly row: PlacementRow | null;
  readonly active: VolumeRunView | null;
  /** The newest mirror round that ended, whatever its end. */
  readonly lastSync: VolumeRunView | null;
  /** The newest mirror round that ended done: every mirror held the primary's data as of its end. */
  readonly lastDone: VolumeRunView | null;
  /** The writer's snapshots, newest first; null where its Server can't list them. */
  readonly snapshots?: readonly Snapshot[] | null;
  /** Null where the writer can't serve offsite, or the Volume has none. */
  readonly offsite?: OffsiteCopy | null;
}): VolumeCopies | null {
  const { volume, members, row, active } = input;
  const form = placementForm(volume, members, row);
  if (form === null) return null;
  const observed = observePlacement(members);
  const moving = form.primary.change !== null && row?.staged?.seed === undefined;
  const promoted = moving && appliedMirrors(observed, row).includes(form.primary.server.id);
  const failed = input.lastSync !== null && ["failed", "lost"].includes(input.lastSync.state);
  const silent = new Set(members.filter((member) => !member.answered).map((member) => member.machine.id));
  const doneAt = input.lastDone?.finished_at == null ? null : new Date(input.lastDone.finished_at);
  const target = active === null ? null : runTarget(active);

  const busyOn = (server: MachineName): string | null => {
    if (active === null) return null;
    if (active.kind === "mirror" && target === server) return "Seeding";
    if (active.kind === "delete_mirror" && target === server) return "Removing";
    return null;
  };
  const mirror = (item: PlacementItem, staged: StagedCopy | null): MirrorCopy => {
    const quiet = silent.has(item.server.id);
    return {
      kind: "mirror",
      id: item.server.id,
      server: item.server,
      name: copyName(volume.name, item.server.name),
      refreshedAt: quiet ? doneAt : syncedAt(item.newest, doneAt),
      skipped: quiet ? "silent" : failed ? "failed" : null,
      staged,
      busy: active?.kind === "sync" ? "Refreshing" : busyOn(item.server.name),
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
  const restore = row?.staged?.restore;
  return {
    volume: { id: volume.id, name: volume.name },
    live: {
      server: observed.primary ?? (form.primary.change === null ? form.primary.server : null),
      restore: seed !== undefined ? { label: seed.point.name } : restore !== undefined ? { label: restoreLabel(restore) } : null,
      moveTo: moving ? form.primary.server : null,
      busy: active !== null && ["move", "release", "restore", "seed"].includes(active.kind) ? runWords(active) : null,
      snapshots: input.snapshots ?? null,
    },
    elsewhere: [
      ...form.mirrors.map((item) => mirror(item, stagedOf(item.change))),
      ...promoted ? [mirror(form.primary, "primary")] : [],
      ...input.offsite ? [input.offsite] : [],
      ...form.detached.map(old),
    ],
  };
}

/**
 * A sync with nothing to send makes no snapshot, yet the mirror still matches the primary as of that sync. A mirror
 * with no snapshot has never received data, so an older done sync says nothing about it.
 */
function syncedAt(newest: Date | null, doneAt: Date | null): Date | null {
  if (newest === null || doneAt === null) return newest;
  return doneAt > newest ? doneAt : newest;
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
