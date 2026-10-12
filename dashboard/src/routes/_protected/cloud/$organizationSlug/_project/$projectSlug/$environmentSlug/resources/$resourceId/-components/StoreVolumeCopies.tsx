import { type ReactNode, useState } from "react";
import {
  CameraIcon, ChevronLeftIcon, ChevronRightIcon, CloudIcon, LockIcon, PlusIcon, RefreshCwIcon, Trash2Icon, TriangleAlertIcon, UploadIcon, XIcon,
} from "lucide-react";
import type { EnvironmentRef, MachineId, MachineName } from "@ployz/sdk";
import { ConfirmDialog } from "#/components/confirm-dialog";
import { Alert, AlertAction, AlertDescription, AlertTitle } from "#/components/ui/alert";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "#/components/ui/dialog";
import { Empty, EmptyDescription } from "#/components/ui/empty";
import { Field, FieldContent, FieldDescription, FieldError, FieldGroup, FieldLabel, FieldTitle } from "#/components/ui/field";
import { Input } from "#/components/ui/input";
import { Item, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemTitle } from "#/components/ui/item";
import { Spinner } from "#/components/ui/spinner";
import { cn } from "#/lib/utils";
import { useInFlightDeployments } from "#/modules/config-store/store-view.queries";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import {
  type AddableCopy, addableCopies, cardLine, COPY_WORDS, isStale, type MirrorCopy, type OffsiteCopy, type OldCopy, safetyLine, STAGED_WORDS,
  type StagedCopy, type VolumeCopies, type VolumeCopy,
} from "#/modules/volume-copies/volume-copies";
import { useRefreshMirror } from "#/modules/volume-copies/volume-copies.hooks";
import { copiesFromPlacement } from "#/modules/volume-copies/volume-copies.placement";
import { type Backup, type BackupChoice, OFFSITE_KEEP, offsiteCopyOf, offsiteServed, scheduleWords } from "#/modules/volume-copies/volume-offsite";
import { useVolumeOffsite, type VolumeOffsiteData } from "#/modules/volume-copies/volume-offsite.hooks";
import { relWords, type SnapshotClock, snapshotClock, snapshotsServed, sizeWords } from "#/modules/volume-copies/volume-snapshots";
import { useSnapshotPolicy, useVolumeSnapshots } from "#/modules/volume-copies/volume-snapshots.hooks";
import { MIRROR_ROUND_MINUTES, mirrorLast, skippedWords } from "#/modules/volume-copies/volume-copies.refresh";
import { placementLacking, placementServers, placementUnknown, restoreOpen } from "#/modules/volume-placement/daemon-release";
import {
  appliedMirrors, editPlacement, failover, lossTime, observePlacement, type Placement, type PlacementEdit, type PlacementRow,
  placementTargets, type PlacementVolume, type RestorePoint, restorePoints, targetPlacement,
} from "#/modules/volume-placement/volume-placement";
import { useDeleteCopy, useStagePlacement, useStageRestore } from "#/modules/volume-placement/volume-placement.hooks";
import { useVolumePlacements } from "#/modules/volume-placement/volume-placement.queries";
import type { Member, VolumeRunView } from "#/modules/volume-run/volume-run";
import { useActiveVolumeRuns, useVolumeMembers } from "#/modules/volume-run/volume-run.queries";
import { useTimeZone } from "#/utils/time-zone";
import type { VolumeScheduleData } from "./StoreVolumeSchedule";
import { RestoreDialog, type RestoreTarget, SnapshotsView, stagedBackup, type VolumeSnapshotsData } from "./StoreVolumeSnapshots";

/** A Volume as the Copies tab shows it: who mounts it names who its Primary runs. */
export type CopiesVolume = PlacementVolume & { readonly mounts: readonly { readonly service: string }[] };

export type CopyServer = { readonly id: string; readonly name: string; readonly storage: "stateless" | "ready" | "pool" | null };

const STAGED_STATE = { new: "success", removing: "destructive", primary: "changed" } as const satisfies Record<StagedCopy, string>;

/** Everything the Copies tab reads and writes, gathered once so the header's safety line and the tab agree. */
export function useVolumeCopies(organizationSlug: string, environment: EnvironmentRef, volume: CopiesVolume) {
  const lens = useRuntimeLens(organizationSlug);
  const members = useVolumeMembers(organizationSlug, environment, volume.id).data;
  const rows = useVolumePlacements(organizationSlug).data ?? [];
  const running = useActiveVolumeRuns(organizationSlug).data?.find((run) => run.volume_id === volume.id) ?? null;
  const placement = useStagePlacement(organizationSlug, environment, volume);
  const restore = useStageRestore(organizationSlug, environment, volume);
  const deleteCopy = useDeleteCopy(organizationSlug, environment, volume);
  const inFlight = useInFlightDeployments(organizationSlug, environment);
  const mirror = useRefreshMirror(organizationSlug, environment, volume.id);
  const clock = snapshotClock(useTimeZone());
  const writer = members === undefined ? null : observePlacement(members).primary?.id ?? null;
  const served = snapshotsServed(lens.machines, writer);
  const snapshots = useVolumeSnapshots(organizationSlug, environment, volume.id, served);
  const schedule = useSnapshotPolicy(organizationSlug, environment, volume.id, served);
  const offsite = useVolumeOffsite(organizationSlug, environment, volume.id, offsiteServed(lens.machines, writer));
  const row = rows.find((entry) => entry.volumeId === volume.id) ?? null;
  const stored = offsite.read?.served ? offsite.read.copy : null;
  const copies = members === undefined ? null : copiesFromPlacement({
    volume, members, row, active: running, lastSync: mirror.lastSync, lastDone: mirror.lastDone, snapshots: snapshots.list?.snapshots ?? null,
    offsite: stored === null ? null : offsiteCopyOf(stored, offsite.uploading ? "Uploading" : null),
  });
  const accepting = placementServers(lens.machines);
  return {
    volume,
    members,
    rows,
    running,
    copies,
    servers: lens.machines.filter((machine) => machine.acceptsServices),
    accepting: members?.filter((member) => accepting.has(member.machine.id)).map((member) => member.machine.id) ?? [],
    upgrade: placementLacking(lens.machines),
    unknown: placementUnknown(lens.machines),
    seeds: members !== undefined && restoreOpen(lens.machines, observePlacement(members).primary?.id ?? null),
    disabled: placement.pending || restore.pending,
    deploying: row?.deploymentId != null && inFlight.some((deployment) => deployment.id === row.deploymentId),
    stage: placement.stage,
    restore: restore.restore,
    deleteCopy,
    refresh: mirror.refresh,
    refreshing: mirror.pending,
    snapshots,
    schedule,
    snapshotClock: clock,
    offsite,
  };
}

export type VolumeCopiesData = ReturnType<typeof useVolumeCopies>;

export type CopiesPanelProps = {
  volume: CopiesVolume;
  members: readonly Member[] | undefined;
  rows: readonly PlacementRow[];
  running: VolumeRunView | null;
  copies: VolumeCopies | null;
  servers: readonly CopyServer[];
  /** Servers that accept Services and run the volume verbs: the only ones a move or a mirror may name. */
  accepting: readonly MachineId[];
  upgrade: readonly string[];
  unknown: readonly string[];
  /** Whether the Primary's daemon can seed a restore. */
  seeds: boolean;
  disabled: boolean;
  /** A Deploy still runs the Volume's placement: the server refuses edits and deletes until it ends. */
  deploying: boolean;
  stage: (staged: Placement | null, mirrors: readonly MachineId[]) => void;
  restore: (point: RestorePoint) => void;
  deleteCopy: (server: MachineName) => void;
  refresh: () => void;
  /** From the Refresh now click until its round ends. */
  refreshing: boolean;
  snapshots: VolumeSnapshotsData;
  schedule: VolumeScheduleData;
  snapshotClock: SnapshotClock;
  offsite: VolumeOffsiteData;
  open: MachineId | null;
  onOpen: (copy: MachineId | null) => void;
  now?: Date;
};

export function SafetyLine({ copies, now = new Date() }: { copies: VolumeCopies; now?: Date }) {
  const line = safetyLine(copies, now);
  return <p className="flex items-center gap-1.5 text-sm" data-tone={line.tone}>
    <span aria-hidden="true" className={cn("size-2 shrink-0 rounded-full", line.tone === "ok" ? "bg-success" : "bg-warning")} />
    <span>{line.words}</span>
    {line.detail ? <span className="text-muted-foreground">· {line.detail}</span> : null}
  </p>;
}

/** A Volume's copies: the live one, and every place it survives if that Server dies. A mirror opens in detail. */
export function VolumeCopiesPanel(props: CopiesPanelProps) {
  const { volume, members, rows, copies, open, onOpen } = props;
  const [snapshotsOpen, setSnapshotsOpen] = useState(false);
  const [offsiteOpen, setOffsiteOpen] = useState(false);
  if (members === undefined) return <Empty variant="placeholder"><EmptyDescription>Waiting for your servers.</EmptyDescription></Empty>;
  const row = rows.find((entry) => entry.volumeId === volume.id) ?? null;
  const target = targetPlacement(observePlacement(members), row);
  if (copies === null || target === null) return <Empty variant="placeholder"><EmptyDescription>No server reports a copy.</EmptyDescription></Empty>;
  const mirror = copies.elsewhere.find((copy): copy is MirrorCopy => copy.kind === "mirror" && copy.id === open);
  if (mirror !== undefined) return <CopyView {...props} members={members} row={row} copies={copies} copy={mirror} onBack={() => onOpen(null)} />;
  const offsite = copies.elsewhere.find((copy): copy is OffsiteCopy => copy.kind === "offsite");
  const read = props.offsite.read;
  if (offsiteOpen && offsite !== undefined && read?.served && read.copy !== null) {
    return <OffsiteView {...props} members={members} row={row} copies={copies} copy={offsite} backups={read.copy.backups} source={read.source}
      restoreTo={restoreTo({ ...props, members, row, target })} onBack={() => setOffsiteOpen(false)} />;
  }
  if (snapshotsOpen && copies.live.snapshots !== null) {
    return <SnapshotsView volume={volume.name} services={mountedServices(volume)} live={copies.live.server?.name ?? "its server"} snapshots={props.snapshots}
      schedule={props.schedule} clock={props.snapshotClock} staged={row?.staged?.restore?.from ?? null} restoreTo={restoreTo({ ...props, members, row, target })}
      offsite={offsite !== undefined} stage={props.stage} onBack={() => setSnapshotsOpen(false)} now={props.now} />;
  }
  return <CopiesView {...props} members={members} row={row} copies={copies} target={target} onSnapshots={() => setSnapshotsOpen(true)}
    onOffsite={() => setOffsiteOpen(true)} />;
}

/** Where a snapshot of this Volume restores now, or null; a failed Deployment's Restore data reads it outside the panel. */
export function snapshotRestoreTarget(data: Pick<VolumeCopiesData, "volume" | "members" | "rows" | "upgrade" | "unknown" | "disabled" | "deploying">) {
  if (data.members === undefined) return null;
  const row = data.rows.find((entry) => entry.volumeId === data.volume.id) ?? null;
  const target = targetPlacement(observePlacement(data.members), row);
  return target === null ? null : restoreTo({ ...data, members: data.members, row, target });
}

type Resolved = CopiesPanelProps & { members: readonly Member[]; row: PlacementRow | null; copies: VolumeCopies };

export const mountedServices = (volume: CopiesVolume) => [...new Set(volume.mounts.map((mount) => mount.service))].join(", ") || volume.name;

/** A snapshot restores onto the writer as it stands: not while a move, a seed or a Deploy has the Volume. */
function restoreTo({ members, row, target, upgrade, unknown, disabled, deploying }: Pick<CopiesPanelProps, "upgrade" | "unknown" | "disabled" | "deploying"> & {
  members: readonly Member[];
  row: PlacementRow | null;
  target: Placement;
}): RestoreTarget | null {
  const observed = observePlacement(members);
  const free = !disabled && !deploying && upgrade.length === 0 && unknown.length === 0;
  if (!free || target.seed !== undefined || observed.primary?.id !== target.primary) return null;
  return { target, applied: appliedMirrors(observed, row) };
}

function CopiesView({ volume, members, row, rows, copies, target, servers, accepting, upgrade, unknown, seeds, disabled, deploying, stage, restore, deleteCopy,
  offsite, onOpen, onSnapshots, onOffsite, now = new Date() }: Resolved & { target: Placement; onSnapshots: () => void; onOffsite: () => void }) {
  const [adding, setAdding] = useState(false);
  const [deleting, setDeleting] = useState<OldCopy | null>(null);
  const observed = observePlacement(members);
  const applied = appliedMirrors(observed, row);
  const blocked = upgrade.length > 0 || unknown.length > 0;
  const locked = disabled || deploying || blocked;
  const edit = (change: PlacementEdit) => stage(editPlacement(target, change), applied);
  const offer = failover(observed, row);
  const mirrorOn = placementTargets(volume, members, row, accepting).mirror.filter((server) => server.id !== target.primary);
  const offered = (["mirror", "offsite"] as const).filter((kind) => kind === "mirror"
    ? mirrorOn.length > 0 || copies.elsewhere.some((copy) => copy.kind === "mirror")
    : offsite.read?.served === true);
  const open = addableCopies(copies, offered);
  const canAdd = !blocked && open.length > 0;
  const { live } = copies;
  const liveName = live.server?.name ?? "its server";

  return <div className="flex flex-col gap-4">
    {offer ? <Alert variant="destructive">
      <AlertTitle>{offer.down.name} is not answering</AlertTitle>
      <AlertDescription>{offer.lostAfter ? `Changes after ${lossTime(offer.lostAfter)} lost` : "Recent changes lost"}</AlertDescription>
      {accepting.includes(offer.mirror.id) ? <AlertAction>
        <Button variant="outline" size="sm" disabled={locked} onClick={() => edit({ kind: "move", to: offer.mirror.id })}>Use mirror</Button>
      </AlertAction> : null}
    </Alert> : null}
    {upgrade.length ? <p className="text-sm text-muted-foreground">Upgrade {upgrade.join(", ")} to a newer Ployz</p>
      : unknown.length ? <p className="text-sm text-muted-foreground">Ployz can’t tell whether {unknown.join(", ")} can move volumes</p> : null}
    <Group label="Live">
      <Item role="listitem" variant="outline" size="sm">
        <ItemContent>
          <ItemTitle>{live.server?.name ?? "Unknown"}</ItemTitle>
          <ItemDescription>{live.busy ? <Busy words={live.busy} /> : live.moveTo ? `Moving to ${live.moveTo.name} · on deploy` : "Primary"}</ItemDescription>
        </ItemContent>
        <ItemActions><Badge variant="success">Live</Badge></ItemActions>
      </Item>
      {live.restore ? <Item role="listitem" variant="outline" size="sm" state="changed" data-staged="restore">
        <ItemContent>
          <ItemTitle>Restore</ItemTitle>
          <ItemDescription>{live.restore.label} · on deploy</ItemDescription>
        </ItemContent>
      </Item> : null}
      {live.snapshots !== null ? <div role="listitem">
        <Item variant="outline" size="sm" render={<button type="button" onClick={onSnapshots} />} className="cursor-pointer text-left hover:bg-muted">
          <CameraIcon className="size-4 shrink-0 text-muted-foreground" />
          <ItemContent>
            <ItemTitle>Snapshots<Badge variant="secondary">{live.snapshots.length}</Badge></ItemTitle>
            <ItemDescription>{live.snapshots[0] ? relWords(live.snapshots[0].at, now) : "None yet"}</ItemDescription>
          </ItemContent>
          <ItemActions><ChevronRightIcon className="size-4 text-muted-foreground" /></ItemActions>
        </Item>
      </div> : null}
    </Group>
    <Group label="Elsewhere">
      {copies.elsewhere.length === 0
        ? <p className="text-sm text-muted-foreground">Nothing outside {liveName}. If {liveName} dies, {volume.name} goes with it.</p>
        : copies.elsewhere.map((copy) => <CopyRow key={copy.id} copy={copy} now={now} locked={locked} blocked={blocked}
          onOpen={() => { if (copy.kind === "mirror") onOpen(copy.server.id); else onOffsite(); }} onDelete={copy.kind === "old" ? () => setDeleting(copy) : null}
          restore={copy.kind === "old" && seeds ? restoreOffer(volume, members, rows, copy.server.id, disabled || deploying, restore) : null} />)}
    </Group>
    {canAdd ? <Button variant="outline" className="border-dashed" disabled={locked} onClick={() => setAdding(true)}>
      <PlusIcon data-icon="inline-start" />Add copy
    </Button> : null}
    <AddCopyDialog open={adding} onOpenChange={(next) => { if (!next) offsite.clearField(); setAdding(next); }} volume={volume} live={live.server?.id ?? null}
      servers={servers} offered={offered} addable={open} mirrorOn={mirrorOn.map((server) => server.id)} offsite={offsite}
      onAdd={(server) => { edit({ kind: "add_mirror", server }); setAdding(false); }}
      onAddOffsite={(choice) => { void offsite.add(choice).then((added) => { if (added) setAdding(false); }); }} />
    <ConfirmDialog open={deleting !== null} onOpenChange={(next) => { if (!next) setDeleting(null); }} title="Delete copy" variant="destructive"
      actionLabel="Delete" pendingLabel="Deleting"
      description={deleting ? <div className="flex flex-col gap-3">
        <p className="flex items-center gap-2 text-foreground"><Trash2Icon className="size-4 shrink-0" />
          {[deleting.server.name, volume.name, deleting.since ? lossTime(deleting.since) : null].filter(Boolean).join(" · ")}
        </p>
        <p>This runs now, not on deploy. It can't be undone.</p>
      </div> : null}
      onConfirm={() => { if (deleting) deleteCopy(deleting.server.name); setDeleting(null); }} />
  </div>;
}

function Group({ label, children }: { label: string; children: ReactNode }) {
  return <section className="flex flex-col gap-1.5">
    <h3 className="text-xs font-medium text-muted-foreground">{label}</h3>
    <ItemGroup role="list" aria-label={label} className="gap-1">{children}</ItemGroup>
  </section>;
}

function Busy({ words }: { words: string }) {
  return <span className="inline-flex items-center gap-1.5"><Spinner role="presentation" aria-hidden="true" />{words}</span>;
}

/** A Server's latest restore point, and whether Restore on it stages, as a mirror's detail and an old copy's row offer it. */
function restoreOffer(volume: CopiesVolume, members: readonly Member[], rows: readonly PlacementRow[], server: MachineId, locked: boolean,
  restore: (point: RestorePoint) => void) {
  const point = restorePoints(members).find((candidate) => candidate.server.id === server) ?? null;
  if (point === null) return null;
  const restoring = rows.filter((entry) => entry.staged?.seed?.sourceVolumeId === volume.id);
  return {
    point,
    staged: restoring.some((entry) => entry.staged?.seed?.point.guid === point.guid),
    disabled: locked || restoring.length > 0,
    stage: () => restore(point),
  };
}

type RestoreOffer = ReturnType<typeof restoreOffer>;

/** One copy away from the live Server. A mirror opens; an old copy restores its point on deploy, or goes at once. */
function CopyRow({ copy, now, locked, blocked, onOpen, onDelete, restore = null }: {
  copy: VolumeCopy;
  now: Date;
  locked: boolean;
  blocked: boolean;
  onOpen: () => void;
  onDelete: (() => void) | null;
  restore?: RestoreOffer;
}) {
  const line = cardLine(copy, now);
  const title = copy.kind === "offsite" ? copy.bucket : copy.server.name;
  const description = line.spin ? <Busy words={line.words} />
    : copy.staged === "new" ? `${COPY_WORDS[copy.kind]} · on deploy`
    : <>
      {[line.words, copy.kind === "mirror" ? "15 min" : copy.kind === "offsite" ? scheduleWords(copy.keep) : null].filter(Boolean).join(" · ")}
      {line.age ? <> · <span className={cn(line.stale && "text-warning")}>{line.stale ? <TriangleAlertIcon className="mr-1 inline size-3.5" /> : null}{copy.kind === "old" && copy.since ? lossTime(copy.since) : line.age}</span></> : null}
    </>;
  const badge = copy.staged ? <Badge variant={STAGED_STATE[copy.staged]}>{STAGED_WORDS[copy.staged]}</Badge> : null;
  const content = <>
    <ItemContent>
      <ItemTitle className={cn(copy.staged === "removing" && "line-through")}>{title}</ItemTitle>
      <ItemDescription>{description}</ItemDescription>
    </ItemContent>
    <ItemActions>
      {badge}
      {copy.kind === "old" ? null : <ChevronRightIcon className="size-4 text-muted-foreground" />}
      {restore?.staged ? <span className="text-sm text-muted-foreground">Staged</span>
        : restore ? <Button variant="outline" size="xs" aria-label={`Restore from ${title}`} disabled={restore.disabled} onClick={restore.stage}>Restore</Button> : null}
      {onDelete && copy.staged === null && copy.busy === null && !blocked
        ? <Button variant="ghost" size="icon-xs" aria-label={`Delete copy on ${title}`} disabled={locked} onClick={onDelete}><XIcon /></Button> : null}
    </ItemActions>
  </>;
  const state = copy.staged ? STAGED_STATE[copy.staged] : undefined;
  if (copy.kind !== "old") {
    return <div role="listitem" data-staged={copy.staged ?? undefined}>
      <Item variant="outline" size="sm" state={state} render={<button type="button" aria-label={`${COPY_WORDS[copy.kind]} on ${title}`} onClick={onOpen} />}
        className="cursor-pointer text-left hover:bg-muted">
        {content}
      </Item>
    </div>;
  }
  return <Item role="listitem" variant="outline" size="sm" state={state} data-staged={copy.staged ?? undefined}
    className={cn(copy.kind === "old" && copy.staged === null && "opacity-70")}>{content}</Item>;
}

function AddCopyDialog({ open, onOpenChange, volume, live, servers, offered, addable, mirrorOn, offsite, onAdd, onAddOffsite }: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  volume: CopiesVolume;
  live: MachineId | null;
  servers: readonly CopyServer[];
  /** The kinds this Volume can have; those it already has read Added. */
  offered: readonly AddableCopy[];
  addable: readonly AddableCopy[];
  mirrorOn: readonly MachineId[];
  offsite: VolumeOffsiteData;
  onAdd: (server: MachineId) => void;
  onAddOffsite: (choice: BackupChoice) => void;
}) {
  const [kind, setKind] = useState<AddableCopy | null>(null);
  const [picked, setPicked] = useState<MachineId | null>(null);
  const [bucket, setBucket] = useState<string | null>(null);
  const [typed, setTyped] = useState({ endpoint: "", bucket: "", accessKey: "", secretKey: "" });
  const buckets = offsite.read?.served ? offsite.read.buckets : [];
  const chosen = kind ?? addable[0] ?? null;
  const into = bucket ?? buckets[0]?.id ?? "new";
  const choice: BackupChoice | null = into !== "new" ? { kind: "existing", id: into }
    : Object.values(typed).every((value) => value.trim() !== "") ? { kind: "new", ...typed } : null;
  const reset = () => {
    setKind(null);
    setPicked(null);
    setBucket(null);
    setTyped({ endpoint: "", bucket: "", accessKey: "", secretKey: "" });
  };
  const target = (server: CopyServer) => {
    if (server.id === live) return { target: null, note: "Live" };
    if (server.storage === "stateless") return { target: null, note: "Docker only" };
    return { target: mirrorOn.find((id) => id === server.id) ?? null, note: null };
  };
  const type = (key: keyof typeof typed) => (event: { target: { value: string } }) => {
    offsite.clearField();
    setTyped({ ...typed, [key]: event.target.value });
  };
  const ready = chosen === "mirror" ? picked !== null : chosen === "offsite" && choice !== null && !offsite.pending;

  return <Dialog open={open} onOpenChange={(next) => { if (!next) reset(); onOpenChange(next); }}>
    <DialogContent>
      <DialogHeader>
        <DialogTitle>Add copy · {volume.name}</DialogTitle>
        <DialogDescription>{chosen === "offsite" ? "This runs now, not on deploy." : "Stages a change. The first copy starts when you deploy."}</DialogDescription>
      </DialogHeader>
      <div role="radiogroup" aria-label="Copy" className="flex flex-col gap-2">
        {offered.map((option) => {
          const added = !addable.includes(option);
          const on = option === chosen;
          return <div key={option} className={cn("flex flex-col gap-2 rounded-lg border px-3 py-2", on ? "border-ring" : "border-border")}>
            <button type="button" role="radio" aria-checked={on} disabled={added} onClick={() => setKind(option)}
              className="flex flex-col text-left disabled:opacity-50">
              <span className="flex items-center gap-2 text-sm font-medium">{COPY_WORDS[option]}{added ? <Badge variant="secondary">Added</Badge> : null}</span>
              <span className="text-sm text-muted-foreground">{option === "mirror"
                ? "Another server · 15 min behind · fast failover" : "S3 bucket · daily · survives losing every server"}</span>
            </button>
            {on && option === "mirror" ? <div role="radiogroup" aria-label="Server" className="flex flex-col gap-1">
              {servers.map((server) => {
                const { target: id, note } = target(server);
                const checked = id !== null && picked === id;
                return <Choice key={server.id} checked={checked} disabled={id === null} onClick={() => setPicked(id)} name={server.name} note={note} />;
              })}
            </div> : null}
            {on && option === "offsite" ? <div className="flex flex-col gap-2">
              <div role="radiogroup" aria-label="Bucket" className="flex flex-col gap-1">
                {buckets.map((entry) => <Choice key={entry.id} checked={into === entry.id} onClick={() => setBucket(entry.id)} name={entry.bucket} note={entry.region} />)}
                <Choice checked={into === "new"} onClick={() => setBucket("new")} name="New bucket" note={null} />
              </div>
              {into === "new" ? <FieldGroup className="gap-3 pl-6">
                <Field><FieldLabel htmlFor="offsite-endpoint">Endpoint</FieldLabel>
                  <Input id="offsite-endpoint" placeholder="s3.eu-central-1.amazonaws.com" autoComplete="off" value={typed.endpoint} onChange={type("endpoint")} /></Field>
                <Field><FieldLabel htmlFor="offsite-bucket">Bucket</FieldLabel>
                  <Input id="offsite-bucket" placeholder="acme-offsite" autoComplete="off" value={typed.bucket} onChange={type("bucket")} /></Field>
                <Field><FieldLabel htmlFor="offsite-access">Access key</FieldLabel>
                  <Input id="offsite-access" placeholder="AKIA…" autoComplete="off" value={typed.accessKey} onChange={type("accessKey")} /></Field>
                <Field data-invalid={offsite.field !== null || undefined}>
                  <FieldLabel htmlFor="offsite-secret">Secret key<Badge variant="secondary"><LockIcon data-icon="inline-start" />Sealed</Badge></FieldLabel>
                  <Input id="offsite-secret" type="password" placeholder="••••••••" autoComplete="off" value={typed.secretKey} onChange={type("secretKey")}
                    aria-invalid={offsite.field !== null || undefined} />
                  {offsite.field === null ? null : <FieldError>{offsite.field}</FieldError>}
                </Field>
              </FieldGroup> : null}
              <Row title="Schedule"><span className="text-muted-foreground">{scheduleWords(OFFSITE_KEEP)}</span></Row>
            </div> : null}
          </div>;
        })}
      </div>
      <DialogFooter>
        <Button variant="outline" onClick={() => onOpenChange(false)}>Cancel</Button>
        <Button disabled={!ready} onClick={() => {
          if (chosen === "mirror" && picked !== null) {
            onAdd(picked);
            reset();
          } else if (chosen === "offsite" && choice !== null) onAddOffsite(choice);
        }}>{offsite.pending && chosen === "offsite" ? <Spinner role="presentation" aria-hidden="true" data-icon="inline-start" /> : null}Add copy</Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>;
}

function Choice({ checked, disabled = false, onClick, name, note }: {
  checked: boolean;
  disabled?: boolean;
  onClick: () => void;
  name: string;
  note: string | null;
}) {
  return <button type="button" role="radio" aria-checked={checked} disabled={disabled} onClick={onClick}
    className={cn("flex items-center justify-between rounded-md border px-3 py-2 text-left text-sm disabled:opacity-50",
      checked ? "border-ring bg-muted" : "border-border")}>
    <span>{name}</span>
    {note ? <span className="text-muted-foreground">{note}</span> : null}
  </button>;
}

/** The bucket copy: Upload now runs at once on the writer; a backup restores through the same staged swap as a snapshot. */
function OffsiteView({ volume, row, copies, copy, backups, source, restoreTo, offsite, snapshots, snapshotClock: clock, stage, onBack, now = new Date() }:
  Resolved & { copy: OffsiteCopy; backups: readonly Backup[] | null; source: string | null; restoreTo: RestoreTarget | null; onBack: () => void }) {
  const [restoring, setRestoring] = useState<Backup | null>(null);
  const epoch = snapshots.list?.epoch ?? null;
  const staged = row?.staged?.restore?.from ?? null;
  const label = (backup: Backup) => `${COPY_WORDS.offsite} · ${clock.when(backup.at, now)}`;
  const uploading = copy.busy !== null;

  return <div className="flex flex-col gap-4">
    <Button variant="ghost" size="sm" className="self-start" onClick={onBack}><ChevronLeftIcon data-icon="inline-start" />Copies</Button>
    <div className="flex min-w-0 items-center gap-2">
      <CloudIcon className="size-4 shrink-0 text-muted-foreground" />
      <span className="font-semibold">{COPY_WORDS.offsite}</span>
      <span className="truncate text-sm text-muted-foreground">{copy.bucket} · {copy.region}</span>
    </div>
    <Row title="Schedule">{scheduleWords(copy.keep)}</Row>
    <Row title="Last">{uploading ? <Busy words="Uploading" /> : copy.uploadedAt ? relWords(copy.uploadedAt, now) : "None yet"}</Row>
    <div className="flex flex-wrap gap-2">
      <Button variant="outline" disabled={offsite.pending || source === null} onClick={() => { void offsite.upload(); }}>
        {uploading ? <Spinner role="presentation" aria-hidden="true" data-icon="inline-start" /> : <UploadIcon data-icon="inline-start" />}Upload now
      </Button>
    </div>
    <Group label="Backups">
      {backups === null ? <p className="text-sm text-muted-foreground">Can’t list backups right now.</p>
        : backups.length === 0 ? <p className="text-sm text-muted-foreground">No backups yet.</p>
        : backups.map((backup) => <Item key={backup.id} role="listitem" variant="outline" size="sm" state={staged === backup.id ? "changed" : undefined}
          data-staged={staged === backup.id ? "restore" : undefined}>
          <ItemContent className="min-w-0">
            <ItemTitle className="gap-2">
              <span className="tabular-nums">{clock.when(backup.at, now)}</span>
              {backup.note ? <span className="truncate font-normal text-muted-foreground">{backup.note}</span> : null}
            </ItemTitle>
          </ItemContent>
          <ItemActions>
            <span className="text-sm text-muted-foreground tabular-nums">{sizeWords(backup.bytes)}</span>
            {staged === backup.id ? <Badge variant="changed">Staged</Badge>
              : restoreTo !== null && epoch !== null ? <Button variant="outline" size="xs" onClick={() => setRestoring(backup)}>Restore</Button> : null}
          </ItemActions>
        </Item>)}
    </Group>
    <RestoreDialog volume={volume.name} services={mountedServices(volume)} live={copies.live.server?.name ?? "its server"} point={restoring === null ? null : { ...restoring, losesBytes: 0 }}
      label={restoring ? label(restoring) : ""} downloads={restoring?.bytes ?? null} now={now}
      onOpenChange={(open) => { if (!open) setRestoring(null); }}
      onRestore={(backup) => {
        if (restoreTo !== null && epoch !== null) stage(...stagedBackup(restoreTo, backup, copy.id, epoch));
        setRestoring(null);
      }} />
  </div>;
}

/** Its staged actions build the same placement the list would; only Details discards them. */
function CopyView({ volume, members, row, rows, copies, copy, accepting, upgrade, unknown, seeds, disabled, deploying, stage, restore, refresh, refreshing,
  onBack, now = new Date() }:
  Resolved & { copy: MirrorCopy; onBack: () => void }) {
  const blocked = upgrade.length > 0 || unknown.length > 0;
  const observed = observePlacement(members);
  const target = targetPlacement(observed, row);
  const applied = appliedMirrors(observed, row);
  // A staged mirror is Details' to discard, and a Deploy running the placement holds it.
  const offered = !blocked && copy.staged === null && !deploying ? target : null;
  const movable = placementTargets(volume, members, row, accepting).move.some((machine) => machine.id === copy.server.id);
  const promote = offered !== null && movable ? editPlacement(offered, { kind: "move", to: copy.server.id }) : null;
  const remove = offered === null ? null : editPlacement(offered, { kind: "remove_mirror", server: copy.server.id });
  const locked = disabled || deploying;
  const live = copies.live.server?.name ?? "the live server";
  const mounted = mountedServices(volume);
  const stale = isStale(copy, now);
  const shown = refreshing && copy.busy === null ? { ...copy, busy: "Refreshing" } : copy;
  const last = mirrorLast(shown, now);
  const skipped = skippedWords(shown, now);
  const offer = restoreOffer(volume, members, rows, copy.server.id, locked, restore);

  return <div className="flex flex-col gap-4">
    <Button variant="ghost" size="sm" className="self-start" onClick={onBack}><ChevronLeftIcon data-icon="inline-start" />Copies</Button>
    <div className="flex min-w-0 items-center gap-2">
      <span className="font-semibold">{COPY_WORDS.mirror}</span>
      <span className={cn("truncate text-muted-foreground", copy.staged === "removing" && "line-through")}>{copy.name}</span>
      {copy.staged ? <Badge variant={STAGED_STATE[copy.staged]}>{STAGED_WORDS[copy.staged]}</Badge> : null}
    </div>
    <Row title="Server">{copy.server.name}</Row>
    <Row title="Refresh">Every {MIRROR_ROUND_MINUTES} min · on deploy</Row>
    <Row title="Last">
      {last.spin ? <Busy words={last.words} />
        : <span className={cn(last.stale && "text-warning")}>{last.stale ? <TriangleAlertIcon className="mr-1 inline size-3.5" /> : null}{last.words}</span>}
    </Row>
    {skipped === null ? null : <Field orientation="responsive">
      <FieldContent><FieldTitle>Status</FieldTitle><FieldDescription>{skipped}</FieldDescription></FieldContent>
      <Badge variant="warning">Refresh skipped</Badge>
    </Field>}
    {offer ? <Row title="Restore point">
      <span className="flex items-center gap-2">
        {lossTime(offer.point.at)}
        {offer.staged ? <span className="text-muted-foreground">Staged</span>
          : seeds ? <Button variant="outline" size="sm" disabled={offer.disabled} onClick={offer.stage}>Restore</Button> : null}
      </span>
    </Row> : null}
    <div data-slot="copy-actions" className="flex flex-wrap gap-2">
      {copy.staged === "new" ? null : <Button variant="outline" disabled={disabled || last.spin} onClick={refresh}>
        {last.spin ? <Spinner role="presentation" aria-hidden="true" data-icon="inline-start" /> : <RefreshCwIcon data-icon="inline-start" />}Refresh now
      </Button>}
      {promote ? <Button variant="outline" disabled={locked} onClick={() => stage(promote, applied)}>Make primary</Button> : null}
    </div>
    {promote ? <p className="text-sm text-muted-foreground">
      {stale
        ? `Make primary moves ${mounted} to ${copy.server.name} on deploy. The deploy catches up first, then pauses writes for the final sync. If ${copy.server.name} can't catch up, the deploy fails and ${live} keeps serving.`
        : `Make primary moves ${mounted} to ${copy.server.name} on deploy. Writes pause for the final sync.`}
    </p> : null}
    {remove ? <section className="flex flex-col gap-2 border-t pt-4">
      <h3 className="text-sm font-medium text-destructive">Remove</h3>
      <p className="text-sm text-muted-foreground">Deletes the copy on {copy.server.name} when you deploy.</p>
      <Button variant="destructive" className="self-start" disabled={locked} onClick={() => stage(remove, applied)}>
        <Trash2Icon data-icon="inline-start" />Remove mirror
      </Button>
    </section> : null}
  </div>;
}

function Row({ title, children }: { title: string; children: ReactNode }) {
  return <Field orientation="responsive">
    <FieldContent><FieldTitle>{title}</FieldTitle></FieldContent>
    <span className="text-sm">{children}</span>
  </Field>;
}

