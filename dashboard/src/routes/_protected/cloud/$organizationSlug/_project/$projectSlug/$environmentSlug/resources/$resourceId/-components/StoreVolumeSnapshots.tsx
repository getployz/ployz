import { type ReactNode, useState } from "react";
import { ArrowLeftRightIcon, CameraIcon, CheckIcon, ChevronLeftIcon, DownloadIcon, LockIcon, MoreHorizontalIcon, Trash2Icon, UploadIcon } from "lucide-react";
import type { MachineId } from "@ployz/sdk";
import { ConfirmDialog } from "#/components/confirm-dialog";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Dialog, DialogContent, DialogFooter, DialogHeader, DialogTitle } from "#/components/ui/dialog";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "#/components/ui/dropdown-menu";
import { Field, FieldContent, FieldTitle } from "#/components/ui/field";
import { Item, ItemActions, ItemContent, ItemGroup, ItemTitle } from "#/components/ui/item";
import { cn } from "#/lib/utils";
import { scheduleWords } from "#/modules/volume-copies/snapshot-schedule";
import type { Snapshot } from "#/modules/volume-copies/volume-copies";
import type { SnapshotList } from "#/modules/volume-copies/volume-snapshots.hooks";
import type { Backup } from "#/modules/volume-copies/volume-offsite";
import { lossWords, type SnapshotClock, SNAPSHOT_WORDS, sizeWords, spanWords } from "#/modules/volume-copies/volume-snapshots";
import type { Placement, SnapshotRestore } from "#/modules/volume-placement/volume-placement";
import { useScheduleEditor, type VolumeScheduleData } from "./StoreVolumeSchedule";

export type RestoreTarget = { readonly target: Placement; readonly applied: readonly MachineId[] };

/**
 * The staged swap that brings `snapshot` back on the next Deploy; `epoch` is the list it was read from, and `save` names
 * the Before restore snapshot of the data it replaces.
 */
export const stagedSnapshot = ({ target, applied }: RestoreTarget, snapshot: Snapshot, epoch: number, save: string = crypto.randomUUID()) => {
  const restore: SnapshotRestore = {
    from: snapshot.id, at: Math.floor(snapshot.at.getTime() / 1000), losesBytes: snapshot.losesBytes, save, expect: epoch,
  };
  return [{ ...target, restore }, applied] as const;
};

/** The staged swap that downloads `backup` from the bucket and brings it back on the next Deploy. */
export const stagedBackup = ({ target, applied }: RestoreTarget, backup: Backup, bucket: string, epoch: number) => {
  const restore: SnapshotRestore = {
    from: backup.id, at: Math.floor(backup.at.getTime() / 1000), losesBytes: 0, save: crypto.randomUUID(), expect: epoch,
    offsite: { bucket, bytes: backup.bytes },
  };
  return [{ ...target, restore }, applied] as const;
};

export type VolumeSnapshotsData = {
  readonly list: SnapshotList | null;
  readonly pending: boolean;
  readonly take: () => void;
  readonly keep: (id: string, kept: boolean) => void;
  readonly remove: (id: string) => void;
};

/**
 * One Volume's snapshots, by day. Take, Keep and Delete run now on the writer; Restore stages a swap for the next Deploy.
 * `restoreTo` is the placement a restore stages onto, or null where a restore can't be staged now.
 */
export function SnapshotsView({ volume, services, live, snapshots, schedule, clock, staged, restoreTo, offsite = false, stage, onBack, now = new Date() }: {
  volume: string;
  services: string;
  live: string;
  snapshots: VolumeSnapshotsData;
  schedule: VolumeScheduleData;
  clock: SnapshotClock;
  staged: string | null;
  restoreTo: RestoreTarget | null;
  /** The Volume has an offsite copy, so each snapshot offers Copy offsite. */
  offsite?: boolean;
  stage: (staged: Placement, mirrors: readonly MachineId[]) => void;
  onBack: () => void;
  now?: Date;
}) {
  const [restoring, setRestoring] = useState<Snapshot | null>(null);
  const [deleting, setDeleting] = useState<Snapshot | null>(null);
  const list = snapshots.list?.snapshots ?? [];
  const epoch = snapshots.list?.epoch ?? null;
  const total = list.reduce((sum, snapshot) => sum + snapshot.bytes, 0);
  const label = (snapshot: Snapshot) => `${SNAPSHOT_WORDS[snapshot.kind]} · ${clock.when(snapshot.at, now)}`;
  const editor = useScheduleEditor(volume, schedule, list);

  return <div className="flex flex-col gap-4">
    <Button variant="ghost" size="sm" className="self-start" onClick={onBack}><ChevronLeftIcon data-icon="inline-start" />Copies</Button>
    <div className="flex items-center gap-2">
      <div className="flex min-w-0 flex-1 flex-col">
        <p className="truncate"><span className="font-semibold">Snapshots</span> <span className="text-sm text-muted-foreground">{live} · {sizeWords(total)}</span></p>
        {schedule.policy === null ? null : <p className="text-sm text-muted-foreground">{scheduleWords(schedule.policy)}</p>}
      </div>
      {editor.edit}
      <Button size="sm" disabled={snapshots.pending || epoch === null} onClick={snapshots.take}>
        <CameraIcon data-icon="inline-start" />Take snapshot
      </Button>
    </div>
    {list.length === 0 ? <p className="text-sm text-muted-foreground">No snapshots yet.</p>
      : clock.groups(list, now).map((day) => <section key={day.day} className="flex flex-col gap-1.5">
        <h3 className="text-xs font-medium text-muted-foreground">{day.day}</h3>
        <ItemGroup role="list" aria-label={day.day} className="gap-1">
          {day.snapshots.map((snapshot) => <SnapshotRow key={snapshot.id} snapshot={snapshot} clock={clock}
            staged={staged === snapshot.id} restorable={restoreTo !== null && epoch !== null} disabled={snapshots.pending} offsite={offsite}
            onRestore={() => setRestoring(snapshot)} onKeep={() => snapshots.keep(snapshot.id, !snapshot.kept)} onDelete={() => setDeleting(snapshot)} />)}
        </ItemGroup>
      </section>)}
    {editor.dialog}
    <RestoreDialog volume={volume} services={services} live={live} point={restoring} label={restoring ? label(restoring) : ""} now={now}
      onOpenChange={(open) => { if (!open) setRestoring(null); }}
      onRestore={(snapshot) => {
        if (restoreTo !== null && epoch !== null) stage(...stagedSnapshot(restoreTo, snapshot, epoch));
        setRestoring(null);
      }} />
    <ConfirmDialog open={deleting !== null} onOpenChange={(next) => { if (!next) setDeleting(null); }} title="Delete snapshot" variant="destructive"
      actionLabel="Delete" pendingLabel="Deleting"
      description={deleting ? <div className="flex flex-col gap-3">
        <p className="flex items-center gap-2 text-foreground"><Trash2Icon className="size-4 shrink-0" />{label(deleting)} · {sizeWords(deleting.bytes)}</p>
        <p>This runs now, not on deploy. It can't be undone.</p>
      </div> : null}
      onConfirm={() => { if (deleting) snapshots.remove(deleting.id); setDeleting(null); }} />
  </div>;
}

function SnapshotRow({ snapshot, clock, staged, restorable, disabled, offsite, onRestore, onKeep, onDelete }: {
  snapshot: Snapshot;
  clock: SnapshotClock;
  staged: boolean;
  restorable: boolean;
  disabled: boolean;
  offsite: boolean;
  onRestore: () => void;
  onKeep: () => void;
  onDelete: () => void;
}) {
  const kind = SNAPSHOT_WORDS[snapshot.kind];
  return <Item role="listitem" variant="outline" size="sm" state={staged ? "changed" : undefined} data-staged={staged ? "restore" : undefined}>
    <ItemContent className="min-w-0">
      <ItemTitle className="gap-2">
        <span className="tabular-nums">{clock.time(snapshot.at)}</span>
        <span>{kind}</span>
        {snapshot.kept ? <LockIcon aria-label="Kept" className="size-3.5 text-muted-foreground" /> : null}
        {snapshot.note ? <span className="truncate font-normal text-muted-foreground">{snapshot.note}</span> : null}
      </ItemTitle>
    </ItemContent>
    <ItemActions>
      <span className="text-sm text-muted-foreground tabular-nums">{sizeWords(snapshot.bytes)}</span>
      {staged ? <Badge variant="changed">Staged</Badge>
        : restorable ? <Button variant="outline" size="xs" onClick={onRestore}>Restore</Button> : null}
      <DropdownMenu>
        <DropdownMenuTrigger render={<Button variant="ghost" size="icon-xs" aria-label={`${kind} ${clock.time(snapshot.at)} options`} disabled={disabled} />}>
          <MoreHorizontalIcon />
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" className="w-auto">
          <DropdownMenuItem onClick={onKeep}>{snapshot.kept ? <CheckIcon /> : <LockIcon />}Keep</DropdownMenuItem>
          {offsite ? <DropdownMenuItem disabled><UploadIcon />Copy offsite</DropdownMenuItem> : null}
          <DropdownMenuItem variant="destructive" onClick={onDelete}><Trash2Icon />Delete</DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </ItemActions>
  </Item>;
}

/** A snapshot restores instantly; a backup names what it `downloads` first, and only dates the writes it loses. */
export function RestoreDialog<P extends Pick<Snapshot, "at" | "losesBytes">>({ volume, services, live, point, label, downloads = null, now, onOpenChange, onRestore }: {
  volume: string;
  services: string;
  live: string;
  point: P | null;
  label: string;
  downloads?: number | null;
  now: Date;
  onOpenChange: (open: boolean) => void;
  onRestore: (point: P) => void;
}) {
  const loses = point === null ? null
    : downloads === null ? lossWords(point, now) : `${spanWords(now.getTime() - point.at.getTime())} of writes`;
  return <Dialog open={point !== null} onOpenChange={onOpenChange}>
    <DialogContent>
      <DialogHeader>
        <DialogTitle className="flex items-center gap-2">Restore {volume}
          {downloads === null ? <Badge variant="success">Instant</Badge>
            : <Badge variant="info"><DownloadIcon data-icon="inline-start" />Downloads {sizeWords(downloads)}</Badge>}
        </DialogTitle>
      </DialogHeader>
      <div className="flex flex-col gap-3">
        <Line title="From">{label}</Line>
        <Line title="Loses"><span className={cn(loses ? "text-destructive" : "text-muted-foreground")}>{loses ?? "Nothing"}</span></Line>
        <section className="flex flex-col gap-1.5">
          <h3 className="text-xs font-medium text-muted-foreground">On deploy</h3>
          <p className="flex items-center gap-2 text-sm"><ArrowLeftRightIcon className="size-4 shrink-0" />{label} swaps in</p>
          <p className="flex items-center gap-2 text-sm text-muted-foreground"><CameraIcon className="size-4 shrink-0" />Current data saved · Before restore</p>
        </section>
        <p className="text-sm text-muted-foreground">Nothing changes until you deploy. {services} restarts on {live}.</p>
      </div>
      <DialogFooter>
        <Button variant="outline" onClick={() => onOpenChange(false)}>Cancel</Button>
        <Button onClick={() => { if (point) onRestore(point); }}>Restore</Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>;
}

function Line({ title, children }: { title: string; children: ReactNode }) {
  return <Field orientation="responsive">
    <FieldContent><FieldTitle>{title}</FieldTitle></FieldContent>
    <span className="text-sm">{children}</span>
  </Field>;
}
