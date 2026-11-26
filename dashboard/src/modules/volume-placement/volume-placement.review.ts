import type { DeletionItem } from "#/modules/config-store/deletion";
import { lossMoment } from "#/utils/relative-time";
import type { ChangeGroup } from "#/modules/config-store/store-deployments";
import { type PlacementVolume, type PlacementChange, detachedName, CHANGE_KINDS, changeSteps, type PlacementLost, type LostCopy, lossByVolume } from "#/modules/volume-placement/volume-placement";

/** Each Volume a Deploy loses, as its confirm lists it, whether Cloud refused it or the canvas checked first. */
export function volumeLossItems(
  volumes: readonly { name: string; deletes: readonly { machine_id: string }[]; loss?: "data" | "writes"; after?: number }[],
  machineName: (id: string) => string | undefined,
): DeletionItem[] {
  return volumes.map((volume) => {
    const servers = volume.deletes.flatMap((held) => machineName(held.machine_id) ?? []).join(", ");
    const writes = volume.after === undefined && volume.loss === "writes" ? "recent changes" : "";
    const item: DeletionItem = { kind: "volume", name: volume.name, detail: [servers, writes].filter(Boolean).join(" · ") || undefined };
    if (volume.after !== undefined) item.lostAfter = new Date(volume.after * 1000);
    return item;
  });
}

export const LOSS_MARK = "#loss-";

/**
 * The Store diff version a Deploy carries, bound to the loss its confirm showed. Names alone would let consent to one
 * loss pass for another staged on the same Volume meanwhile, and staging a copy change doesn't move the Store's version.
 * Each deleted copy counts as its Server reported it, slot, newest snapshot and whether it was written there, so a copy
 * replaced or written since needs a new confirm. The version is the one field every client carries back verbatim.
 */
export function lossVersion(version: string, lost: readonly PlacementLost[]) {
  if (lost.length === 0) return version;
  const copy = ({ machine_id, slot, newest, written_here }: LostCopy) => [machine_id, slot ?? "", newest ?? "", String(written_here ?? "")].join("/");
  const shown = JSON.stringify(lossByVolume(lost)
    .map((volume) => ({ ...volume, deletes: volume.deletes.map(copy).sort() }))
    .sort((a, b) => a.name.localeCompare(b.name)));
  let hash = 0x811c9dc5;
  for (let index = 0; index < shown.length; index++) hash = Math.imul(hash ^ shown.charCodeAt(index), 0x01000193);
  return `${version}${LOSS_MARK}${(hash >>> 0).toString(36)}`;
}

/** The Store's own version inside one `lossVersion` bound. */
export const storeVersion = (version: string) => version.split(LOSS_MARK)[0] ?? version;

/**
 * What a Deploy of staged changes takes, for its typed confirm: what goes, whether a restore drops writes, the name of
 * each Volume that loses something, which the Deploy accepts as it accepts a Volume the Store deletes, and the version
 * binding that acceptance to this loss.
 */
export type PlacementLoss = {
  readonly items: readonly DeletionItem[];
  readonly losesWrites: boolean;
  readonly accept: readonly string[];
  readonly version: (version: string) => string;
};

export function placementLoss(lost: readonly PlacementLost[]): PlacementLoss {
  const volumes = lossByVolume(lost);
  const servers = lost.flatMap(({ server }) => server ?? []);
  return {
    items: volumeLossItems(volumes, (id) => servers.find((server) => server.id === id)?.name),
    losesWrites: volumes.some(({ loss }) => loss === "writes"),
    accept: [...new Set(lost.map(({ volume }) => volume.name))],
    version: (version) => lossVersion(version, lost),
  };
}

export function detailsRow(volume: PlacementVolume, change: PlacementChange, now: Date) {
  const clears = "clears" in change && change.clears.length > 0 ? `Deletes ${detachedName(volume)} first` : undefined;
  switch (change.kind) {
    case "move":
      if (change.how !== "restore") return { name: "Primary", currentValue: change.from?.name ?? "", newValue: change.to.name, detail: clears };
      return {
        name: "Primary", currentValue: change.from?.name ?? "", newValue: change.to.name,
        loss: change.lostAfter === null ? "Recent changes lost" : `Changes after ${lossMoment(change.lostAfter, now)} lost`,
      };
    case "add_mirror":
      return { name: "Mirror", currentValue: "", newValue: change.server.name, detail: clears };
    case "remove_mirror":
      return { name: "Mirror", currentValue: change.server.name, newValue: "" };
    case "delete_copy":
      return { name: "Detached copy", currentValue: change.server.name, newValue: "" };
    case "seed": {
      const at = lossMoment(new Date(change.point.at * 1000), now);
      return {
        name: "From", currentValue: "", newValue: `${change.source.name} · ${at}`,
        loss: `Changes after ${at} are not in ${volume.name}`,
        detail: "Volume data, not database point-in-time.",
      };
    }
  }
}

export function placementGroup(volume: PlacementVolume, changes: readonly PlacementChange[], now: Date, unreached = false): ChangeGroup {
  return {
    owner: "placement",
    nodeType: "volume",
    nodeId: volume.id,
    nodeName: volume.name,
    discardPath: volume.id,
    row: null,
    lifecycle: changes.some((change) => change.kind === "seed") ? "create" : "update",
    changeCount: changes.length,
    restarts: [],
    canDiscard: true,
    unreached,
    rows: changes.map((change, index) => {
      const words = detailsRow(volume, change, now);
      return {
        changeKey: `${volume.id}:placement:${index}`,
        label: words.name,
        ...words,
        steps: changeSteps(change, now),
        variable: false,
        kind: CHANGE_KINDS[change.kind],
        path: String(index),
        row: null,
        canDiscard: true,
      };
    }),
  };
}

/**
 * Details shows a restored Volume once. The Store lists it as a created Volume and its placement row says where the
 * data comes from, so both go into one group. Discard takes the whole group through the placement, which removes
 * the Volume and puts the mount back as one write.
 */
export function mergeRestoreGroups(store: readonly ChangeGroup[], placement: readonly ChangeGroup[]): ChangeGroup[] {
  const restored = new Map(placement.filter((group) => group.lifecycle === "create").map((group) => [group.nodeId, group]));
  const merged = store.map((group) => {
    const seed = restored.get(group.nodeId);
    if (seed === undefined) return group;
    restored.delete(group.nodeId);
    return {
      ...seed,
      rows: [...seed.rows, ...group.rows.map((row) => ({ ...row, canDiscard: false }))],
      changeCount: seed.changeCount + group.changeCount,
      restarts: group.restarts,
    };
  });
  return [...merged, ...placement.filter((group) => group.lifecycle !== "create" || restored.has(group.nodeId))];
}
