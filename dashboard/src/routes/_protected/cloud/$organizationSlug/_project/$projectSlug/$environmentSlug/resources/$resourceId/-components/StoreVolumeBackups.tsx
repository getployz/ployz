import type { EnvironmentRef, VolumeListing } from "@ployz/sdk";
import { RelativeTime } from "#/components/relative-time";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Empty, EmptyDescription, EmptyTitle } from "#/components/ui/empty";
import { Item, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemTitle } from "#/components/ui/item";
import { lossTime, type PlacementRow, type RestorePoint, restorePoints } from "#/modules/volume-placement/volume-placement";
import { useStageRestore } from "#/modules/volume-placement/volume-placement.hooks";
import { useVolumePlacements } from "#/modules/volume-placement/volume-placement.queries";
import type { Member } from "#/modules/volume-run/volume-run";
import { useVolumeMembers } from "#/modules/volume-run/volume-run.queries";
import { SettingsSection } from "#/routes/_protected/cloud/$organizationSlug/-components/SettingsSection";

type BackupsVolume = Pick<VolumeListing, "id" | "name" | "deployed" | "storage">;

/** A Volume's restore points: its mirrors' latest copies. Restore stages a new Volume from one; Details then deploys it. */
export function StoreVolumeBackups({ organizationSlug, environment, volume, removing, onAddMirror }: {
  organizationSlug: string;
  environment: EnvironmentRef;
  volume: BackupsVolume;
  removing: boolean;
  onAddMirror: () => void;
}) {
  const managed = volume.deployed && volume.storage.kind === "provisioned" && !removing;
  const members = useVolumeMembers(organizationSlug, environment, volume.id).data;
  const rows = useVolumePlacements(organizationSlug).data ?? [];
  const { restore, pending } = useStageRestore(organizationSlug, environment, volume);
  return <>
    {managed ? <VolumeRestorePoints volume={volume} members={members} rows={rows} disabled={pending} restore={restore} onAddMirror={onAddMirror} /> : (
      <SettingsSection id="restore-points" title="Restore points">
        <Empty variant="placeholder">
          <EmptyDescription>{removing ? "Removed on next deploy." : volume.deployed ? "Managed volumes only." : "Deploy first."}</EmptyDescription>
        </Empty>
      </SettingsSection>
    )}
    <SettingsSection id="s3" title="S3">
      <Item variant="outline" aria-disabled>
        <ItemContent><ItemTitle>Copies to your bucket</ItemTitle></ItemContent>
        <ItemActions><Badge variant="secondary">Soon</Badge></ItemActions>
      </Item>
    </SettingsSection>
  </>;
}

const SCHEDULE = "Each mirror keeps its latest point. Refreshed on every deploy and every 15 minutes.";

export function VolumeRestorePoints({ volume, members, rows, disabled, restore, onAddMirror }: {
  volume: BackupsVolume;
  members: readonly Member[] | undefined;
  rows: readonly PlacementRow[];
  disabled: boolean;
  restore: (point: RestorePoint) => void;
  onAddMirror: () => void;
}) {
  if (members === undefined) {
    return <SettingsSection id="restore-points" title="Restore points">
      <Empty variant="placeholder"><EmptyDescription>Waiting for your servers.</EmptyDescription></Empty>
    </SettingsSection>;
  }
  const points = restorePoints(members);
  // A staged restore belongs to the new Volume's row; here it only marks its point. Details discards it.
  const restoring = rows.filter((row) => row.staged?.seed?.sourceVolumeId === volume.id);
  const stagedOf = (point: RestorePoint) => restoring.find((row) => row.staged?.seed?.point.guid === point.guid) ?? null;
  const own = rows.find((row) => row.volumeId === volume.id) ?? null;
  // One restore at a time: a second would find the mounts already moved.
  const locked = disabled || own?.deploymentId != null || restoring.length > 0;
  return <SettingsSection id="restore-points" title="Restore points" description={points.length > 0 ? SCHEDULE : undefined}>
    {points.length === 0 ? (
      <Empty variant="placeholder">
        <EmptyTitle>No restore points</EmptyTitle>
        <EmptyDescription>
          A mirror makes one on every deploy and every 15 minutes.{" "}
          <button type="button" className="underline underline-offset-4" onClick={onAddMirror}>Add mirror</button>
        </EmptyDescription>
      </Empty>
    ) : (
      <ItemGroup role="list" aria-label="Restore points" className="gap-1">
        {points.map((point) => {
          const staged = stagedOf(point);
          return <Item key={point.guid} role="listitem" variant="outline" size="sm" className="group/point">
            <ItemContent>
              <ItemTitle>{lossTime(point.at)}</ItemTitle>
              <ItemDescription><RelativeTime date={point.at} /> · {point.server.name}</ItemDescription>
            </ItemContent>
            <ItemActions>
              {staged ? <span className="text-sm text-muted-foreground">Staged</span> : (
                <Button variant="outline" size="sm" disabled={locked} onClick={() => restore(point)}
                  className="opacity-0 transition-opacity group-focus-within/point:opacity-100 group-hover/point:opacity-100 focus-visible:opacity-100 disabled:opacity-0">
                  Restore
                </Button>
              )}
            </ItemActions>
          </Item>;
        })}
      </ItemGroup>
    )}
  </SettingsSection>;
}
