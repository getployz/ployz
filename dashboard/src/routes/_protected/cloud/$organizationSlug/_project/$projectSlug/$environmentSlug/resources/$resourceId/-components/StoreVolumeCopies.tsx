import { useState } from "react";
import type { EnvironmentRef, VolumeListing } from "@ployz/sdk";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Field, FieldContent, FieldDescription, FieldTitle } from "#/components/ui/field";
import { Item, ItemContent, ItemDescription, ItemTitle, ItemGroup } from "#/components/ui/item";
import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription, DialogFooter } from "#/components/ui/dialog";
import { Alert, AlertTitle, AlertDescription } from "#/components/ui/alert";
import { Spinner } from "#/components/ui/spinner";
import { Empty, EmptyDescription } from "#/components/ui/empty";
import { useWarmMovePreview, previewCopies } from "#/modules/volume-run/WarmMovePreview";
import { Select, SelectContent, SelectGroup, SelectItem, SelectTrigger, SelectValue } from "#/components/ui/select";
import { RelativeTime } from "#/components/relative-time";
import { listNames } from "#/lib/plural";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import { runText, volumeCopies, type CopyRole, type Offer, type VolumeCopies } from "#/modules/volume-run/volume-copies";
import type { VolumeRunView } from "#/modules/volume-run/volume-run";
import { useRequestVolumeRun } from "#/modules/volume-run/volume-run.hooks";
import type { VolumeRunRequest } from "#/modules/volume-run/volume-run.functions";
import { useVolumeMembers, useVolumeRuns } from "#/modules/volume-run/volume-run.queries";
import { RowWarning } from "#/routes/_protected/cloud/$organizationSlug/-components/SettingsSection";

const ROLE = {
  writer: { text: "Writer", variant: "secondary" },
  moving: { text: "Moving", variant: "info" },
  mirror: { text: "Mirror", variant: "info" },
  old: { text: "Old", variant: "outline" },
} as const satisfies Record<CopyRole, { text: string; variant: string }>;

/**
 * Where a deployed managed Volume's copies are, as the Servers report them, and the runs that move them: Mirror, Sync,
 * Move, Release, Restore. One run at a time; its progress and the last runs show below.
 */
type StoreVolumeCopiesProps = { organizationSlug: string; environment: EnvironmentRef; volume: VolumeListing };
export function StoreVolumeCopies(props: StoreVolumeCopiesProps) {
  const preview = useWarmMovePreview();
  if (preview) return <VolumeCopiesPanel {...previewCopies(preview, props.volume.name)} observed pending={false} request={preview.start} />;
  return <LiveStoreVolumeCopies {...props} />;
}

function LiveStoreVolumeCopies({ organizationSlug, environment, volume }: StoreVolumeCopiesProps) {
  const lens = useRuntimeLens(organizationSlug);
  const { data: runs = [] } = useVolumeRuns(organizationSlug, volume.id);
  const members = useVolumeMembers(organizationSlug, environment, volume.id);
  const { request, pending } = useRequestVolumeRun(organizationSlug, environment, volume);
  return <VolumeCopiesPanel view={volumeCopies({ volume, members: members.data ?? [], machines: lens.machines, runs })} runs={runs}
    observed={members.data !== undefined} pending={pending} request={request} />;
}


type Action = { kind: "mirror" | "move" | "restore" | "sync" | "release"; servers: readonly string[] };
const ACTION = {
  mirror: { title: "Mirror", description: "Keep a read-only copy on another server. It updates only when you sync it.", confirm: "Create mirror" },
  sync: { title: "Sync mirror", description: "Send changes from the writer to its mirror. Your service keeps running.", confirm: "Sync mirror" },
  move: { title: "Move volume", description: "Copy changes while your service runs, then stop it briefly to switch servers. The old server keeps a read-only copy.", confirm: "Move volume" },
  release: { title: "Release interrupted move", description: "Start the service again on the original server. The destination copy stays read-only.", confirm: "Release move" },
  restore: { title: "Restore from mirror", description: "Make this mirror the writer. Writes after its last sync are lost. Deploy afterward to start the service.", confirm: "Restore volume" },
} as const;

export function VolumeCopiesPanel({ view, runs, observed, pending, request, progress = null }: {
  view: VolumeCopies; runs: readonly VolumeRunView[]; observed: boolean; pending: boolean; request: (run: VolumeRunRequest) => void; progress?: string | null;
}) {
  const [action, setAction] = useState<Action | null>(null);
  const [server, setServer] = useState("");
  const { offers } = view;
  const busy = pending || view.active !== null;
  function choose(kind: Action["kind"], offer: Offer) {
    if (!offer) return;
    setServer(offer.servers[0] ?? "");
    setAction({ kind, servers: offer.servers });
  }
  function confirm() {
    if (!action || busy || !offers[action.kind] || ((action.kind === "mirror" || action.kind === "move" || action.kind === "restore") && !offers[action.kind]?.servers.includes(server))) return;
    switch (action.kind) {
      case "mirror": case "move": request({ kind: action.kind, args: { to: server } }); break;
      case "restore": request({ kind: "restore", args: { from: server } }); break;
      case "sync": request({ kind: "sync", args: { full: false } }); break;
      case "release": request({ kind: "release", args: {} }); break;
    }
    setAction(null);
  }
  const latest = runs.find((run) => run.state !== "requested" && run.state !== "running");
  return <div className="flex flex-col gap-4">
    {view.active || pending ? <Alert>
      <AlertTitle>{view.active ? runText(view.active).what : "Starting operation"}</AlertTitle>
      <AlertDescription className="flex flex-col gap-3">
        <div role="status" className="flex items-center gap-2"><Spinner role="presentation" aria-hidden="true" /><span>{progress ?? view.active?.message ?? "Waiting for the servers to report progress."}</span></div>
        <p>You can close this panel. The operation continues.</p>
      </AlertDescription>
    </Alert> : null}
    {latest?.kind === "restore" && latest.state === "done" && !busy ? <Alert><AlertTitle>Volume restored</AlertTitle><AlertDescription>Deploy this environment to start the service on its new writer.</AlertDescription></Alert> : null}
    {latest && ["failed", "lost", "cancelled", "not_started"].includes(latest.state) ? <Alert variant="destructive">
      <AlertTitle>{runText(latest).what} · {runText(latest).state}</AlertTitle>
      <AlertDescription>{latest.message ?? "The operation did not finish. Check the copies below before continuing."}</AlertDescription>
    </Alert> : null}
    {view.unanswered.length > 0 ? <RowWarning>{listNames([...view.unanswered])} did not answer. Their copies are unknown. Operations resume when they answer or are removed.</RowWarning> : null}
    {view.twoWriters ? <RowWarning>Two servers report a writer. We cannot tell which is newer. Make one read-only before continuing.</RowWarning> : null}
    {view.sealing ? <Alert><AlertTitle>Old copy returned</AlertTitle><AlertDescription>{view.sealing} must become read-only before another operation.</AlertDescription>
      <Button variant="outline" disabled={busy || !offers.sync} onClick={() => request({ kind: "sync", args: { full: false } })}>Make old copy read-only</Button>
    </Alert> : null}
    {view.copies.length ? <ItemGroup role="list" aria-label="Copies">
      {view.copies.map((copy) => <Item key={copy.machineId} role="listitem">
        <ItemContent><ItemTitle>{copy.label}<Badge variant={ROLE[copy.role].variant}>{ROLE[copy.role].text}</Badge></ItemTitle>
          <ItemDescription>{copy.server}{copy.role === "mirror" ? " · Updates on request" : copy.role === "writer" ? " · Receives service writes" : null}</ItemDescription></ItemContent>
        {copy.role === "mirror" && offers.sync && !view.sealing ? <Button variant="outline" size="sm" disabled={busy} onClick={() => request({ kind: "sync", args: { full: false } })}>Sync</Button> : null}
        {copy.role === "mirror" && offers.restore?.servers.includes(copy.server) ? <Button variant="outline" size="sm" disabled={busy} onClick={() => choose("restore", { servers: [copy.server] })}>Restore</Button> : null}
      </Item>)}
    </ItemGroup> : <Empty variant="placeholder"><EmptyDescription>{observed ? "No server reports a copy." : "Waiting for your servers."}</EmptyDescription></Empty>}
    {offers.release ? <Field orientation="responsive"><FieldContent><FieldTitle>Move interrupted</FieldTitle><FieldDescription>Resume on the original server, or retry the move.</FieldDescription></FieldContent>
      <Button variant="outline" disabled={busy} onClick={() => choose("release", offers.release)}>Release</Button></Field> : null}
    <div className="flex flex-wrap gap-2">
      {offers.mirror ? <Button variant="outline" disabled={busy} onClick={() => choose("mirror", offers.mirror)}>Create mirror</Button> : null}
      {offers.move ? <Button variant="outline" disabled={busy} onClick={() => choose("move", offers.move)}>Move volume</Button> : null}
    </div>
    {!busy && !view.sealing && !view.twoWriters && !view.unanswered.length && !Object.values(offers).some(Boolean) ? <p className="text-sm text-muted-foreground">No operation is available from the current server reports.</p> : null}
    {runs.length ? <div className="flex flex-col gap-3">
      <p className="text-sm font-medium">Activity</p>
      {runs.slice(0, 10).map((run) => <div key={run.id} className="flex flex-col gap-1 border-b pb-3 text-sm">
        <div className="flex items-baseline justify-between gap-2"><span>{runText(run).what}</span><Badge variant={run.state === "failed" || run.state === "lost" ? "destructive" : "outline"}>{runText(run).state}</Badge></div>
        {run.message ? <p className="text-muted-foreground">{run.message}</p> : null}
        <span className="text-xs text-muted-foreground"><RelativeTime date={new Date(run.finished_at ?? run.created_at)} /></span>
      </div>)}
    </div> : null}
    <Dialog open={action !== null} onOpenChange={(open) => { if (!open) setAction(null); }}>
      <DialogContent>
        {action ? <>
          <DialogHeader><DialogTitle>{ACTION[action.kind].title}</DialogTitle><DialogDescription>{ACTION[action.kind].description}</DialogDescription></DialogHeader>
          {action.kind === "mirror" || action.kind === "move" || action.kind === "restore" ? <Field><FieldTitle>{action.kind === "restore" ? "Restore from" : "Destination server"}</FieldTitle>
            <Select value={server} onValueChange={(value) => setServer(value ?? "")}><SelectTrigger aria-label="Destination server"><SelectValue placeholder="Select a server" /></SelectTrigger>
              <SelectContent><SelectGroup>{action.servers.map((name) => <SelectItem key={name} value={name}>{name}</SelectItem>)}</SelectGroup></SelectContent>
            </Select>
          </Field> : null}
          {action.kind === "restore" ? <Alert variant="destructive"><AlertTitle>Recent writes will be lost</AlertTitle><AlertDescription>A mirror is only as current as its last successful sync. Its age is not known from these server reports.</AlertDescription></Alert> : null}
          <DialogFooter><Button variant="outline" onClick={() => setAction(null)}>Cancel</Button><Button variant={action.kind === "restore" ? "destructive" : "default"} disabled={busy || !offers[action.kind] || (action.servers.length > 0 && !offers[action.kind]?.servers.includes(server))} onClick={confirm}>{ACTION[action.kind].confirm}</Button></DialogFooter>
        </> : null}
      </DialogContent>
    </Dialog>
  </div>;
}
