import { useState } from "react";
import type { EnvironmentRef, VolumeListing } from "@ployz/sdk";
import { UndoIcon } from "lucide-react";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Item, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemTitle } from "#/components/ui/item";
import { useCachedStoreView, volumesQuery } from "#/modules/config-store/store-view.queries";
import { deployRestoreSaveId, deploySnapshotId } from "#/modules/volume-copies/snapshot-schedule";
import { SNAPSHOT_WORDS, sizeWords } from "#/modules/volume-copies/volume-snapshots";
import { mountedServices, snapshotRestoreTarget, useVolumeCopies, type VolumeCopiesData } from "../resources/$resourceId/-components/StoreVolumeCopies";
import { RestoreDialog, stagedSnapshot } from "../resources/$resourceId/-components/StoreVolumeSnapshots";

/**
 * A failed Deployment's way back: each Volume whose Before deploy snapshot it took, restorable through the staged
 * Restore. The box shows only once a row qualifies, since each row learns that from its own Volume's reads.
 */
export function DeploymentRestoreData({ organizationSlug, environment, deploymentId }: {
  organizationSlug: string;
  environment: EnvironmentRef;
  deploymentId: string;
}) {
  const result = useCachedStoreView(organizationSlug, volumesQuery(environment));
  const volumes = result?.ok
    ? result.value.volumes.filter((volume) => volume.deployed && volume.storage.kind === "provisioned" && volume.change !== "delete")
    : [];
  if (volumes.length === 0) return null;
  return <section aria-label="Restore data" className="hidden flex-col gap-2 rounded-lg border p-3 has-[[data-restore-row]]:flex">
    <h3 className="flex items-center gap-2 text-sm font-medium"><UndoIcon className="size-4" />Restore data</h3>
    <ItemGroup role="list" className="gap-1">
      {volumes.map((volume) => <RestoreRow key={volume.id} organizationSlug={organizationSlug} environment={environment} volume={volume}
        deploymentId={deploymentId} />)}
    </ItemGroup>
  </section>;
}

function RestoreRow({ organizationSlug, environment, volume, deploymentId }: {
  organizationSlug: string;
  environment: EnvironmentRef;
  volume: VolumeListing;
  deploymentId: string;
}) {
  const data = useVolumeCopies(organizationSlug, environment, volume);
  return <RestoreDataRow deploymentId={deploymentId} data={{ ...data, live: data.copies?.live.server?.name ?? "its server" }} />;
}

export type RestoreRowData = Pick<VolumeCopiesData, "volume" | "members" | "rows" | "upgrade" | "unknown" | "disabled" | "deploying" | "stage" | "snapshotClock"> & {
  readonly snapshots: Pick<VolumeCopiesData["snapshots"], "list">;
  readonly live: string;
};

/**
 * One Volume's row: absent unless this deploy's Before snapshot is listed and a restore can be staged, already is, or
 * ran. The restore's Before restore snapshot, named after this deploy, says it ran and which deploy ran it.
 */
export function RestoreDataRow({ deploymentId, data }: { deploymentId: string; data: RestoreRowData }) {
  const { volume } = data;
  const [open, setOpen] = useState(false);
  const id = deploySnapshotId("before", deploymentId);
  const epoch = data.snapshots.list?.epoch ?? null;
  const snapshot = data.snapshots.list?.snapshots.find((entry) => entry.id === id);
  const restoreTo = snapshotRestoreTarget(data);
  const save = deployRestoreSaveId(deploymentId);
  const restored = data.snapshots.list?.snapshots.find((entry) => entry.id === save);
  const staged = data.rows.find((row) => row.volumeId === volume.id)?.staged?.restore?.from === id;
  if (snapshot === undefined || epoch === null || (restoreTo === null && !staged && restored === undefined)) return null;
  const now = new Date();
  const label = `${SNAPSHOT_WORDS[snapshot.kind]} · ${data.snapshotClock.when(snapshot.at, now)}`;

  return <Item role="listitem" data-restore-row variant="outline" size="sm" state={staged ? "changed" : undefined}>
    <ItemContent className="min-w-0">
      <ItemTitle>{volume.name}</ItemTitle>
      <ItemDescription>{label}</ItemDescription>
    </ItemContent>
    <ItemActions>
      {restored !== undefined ? <Badge variant="success">Restored {restored.note}</Badge> : <>
        <span className="text-sm text-muted-foreground tabular-nums">
          {snapshot.losesBytes > 0 ? `${sizeWords(snapshot.losesBytes)} written since` : "Unchanged"}
        </span>
        {staged ? <Badge variant="changed">Staged</Badge>
          : <Button variant="outline" size="xs" disabled={data.disabled} onClick={() => setOpen(true)}>Restore</Button>}
      </>}
    </ItemActions>
    <RestoreDialog volume={volume.name} services={mountedServices(volume)} live={data.live} snapshot={open ? snapshot : null} label={label} now={now}
      onOpenChange={setOpen}
      onRestore={(chosen) => {
        if (restoreTo !== null) data.stage(...stagedSnapshot(restoreTo, chosen, epoch, save));
        setOpen(false);
      }} />
  </Item>;
}
