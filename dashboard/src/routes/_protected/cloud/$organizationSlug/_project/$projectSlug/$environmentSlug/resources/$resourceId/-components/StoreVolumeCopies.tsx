import { useState } from "react";
import type { EnvironmentRef, VolumeListing } from "@ployz/sdk";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Field, FieldContent, FieldDescription, FieldTitle } from "#/components/ui/field";
import { Item, ItemContent, ItemDescription, ItemTitle } from "#/components/ui/item";
import { Select, SelectContent, SelectGroup, SelectItem, SelectTrigger, SelectValue } from "#/components/ui/select";
import { RelativeTime } from "#/components/relative-time";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import { runText, volumeCopies, type CopyRole, type Offer, type VolumeCopies, type VolumeCopy } from "#/modules/volume-run/volume-copies";
import type { VolumeRunView } from "#/modules/volume-run/volume-run";
import { useRequestVolumeRun } from "#/modules/volume-run/volume-run.hooks";
import type { VolumeRunRequest } from "#/modules/volume-run/volume-run.functions";
import { useVolumeRuns } from "#/modules/volume-run/volume-run.queries";
import { RowWarning } from "#/routes/_protected/cloud/$organizationSlug/-components/SettingsSection";

const ROLE = {
  writer: { text: "Writer", variant: "secondary" },
  moving: { text: "Moving", variant: "changed" },
  mirror: { text: "Mirror", variant: "info" },
  old: { text: "Old", variant: "outline" },
} as const satisfies Record<CopyRole, { text: string; variant: string }>;

/**
 * Where a deployed managed Volume's copies are, as the Servers report them, and the runs that move them: Mirror, Sync,
 * Move, Release, Restore. One run at a time; its progress and the last runs show below.
 */
export function StoreVolumeCopies({ organizationSlug, environment, volume }: {
  organizationSlug: string; environment: EnvironmentRef; volume: VolumeListing;
}) {
  const lens = useRuntimeLens(organizationSlug);
  const { data: runs = [] } = useVolumeRuns(organizationSlug, volume.id);
  const { request, pending } = useRequestVolumeRun(organizationSlug, environment, volume);
  return <VolumeCopiesPanel view={volumeCopies({ volume, copies: lens.volumeCopies, machines: lens.machines, runs })} runs={runs}
    observed={lens.status === "observed"} pending={pending} request={request} />;
}

export function VolumeCopiesPanel({ view, runs, observed, pending, request }: {
  view: VolumeCopies; runs: readonly VolumeRunView[]; observed: boolean; pending: boolean; request: (run: VolumeRunRequest) => void;
}) {
  const { offers } = view;
  const busy = pending || view.active !== null;

  return (
    <div className="flex flex-col gap-6">
      {view.copies.length === 0 ? (
        <p className="text-sm text-muted-foreground">{observed ? "No server reports a copy." : "Waiting for your servers."}</p>
      ) : (
        <div role="list" aria-label="Copies" className="flex flex-col gap-2">
          {view.copies.map((copy) => <CopyItem key={copy.machineId} copy={copy} />)}
        </div>
      )}
      {view.twoWriters ? (
        <RowWarning>Two servers each hold this volume as the writer. Nothing starts here until one of them goes.</RowWarning>
      ) : null}
      {view.active ? (
        <p className="text-sm">{runText(view.active).what} · {runText(view.active).state.toLowerCase()} since <RelativeTime date={new Date(view.active.created_at)} /></p>
      ) : null}
      <ServerAction title="Mirror" description="Keep a read-only copy on another server. It refreshes when you sync it." verb="Mirror"
        offer={offers.mirror} busy={busy} onRun={(to) => request({ kind: "mirror", args: { to } })} />
      {offers.sync ? (
        <RunAction title="Sync" description="Send the mirror what changed since the last sync." busy={busy}
          run={{ kind: "sync", args: { full: false } }} verb="Sync" onRun={request} />
      ) : null}
      <ServerAction title="Move" description="Run it on another server. Its service stops only while the last changes copy over." verb="Move"
        offer={offers.move} busy={busy} onRun={(to) => request({ kind: "move", args: { to } })} />
      {offers.release ? (
        <RunAction title="Release" description="The last move stopped. Start the service again on the old server." busy={busy}
          run={{ kind: "release", args: {} }} verb="Release" onRun={request} />
      ) : null}
      <ServerAction title="Restore" description="Its server is gone. Make the mirror the volume, then deploy. Writes after its last sync are lost." verb="Restore"
        offer={offers.restore} busy={busy} onRun={(from) => request({ kind: "restore", args: { from } })} />
      {runs.length > 0 ? (
        <div className="flex flex-col gap-1">
          <p className="text-sm font-medium">Activity</p>
          {runs.slice(0, 10).map((run) => {
            const text = runText(run);
            return (
              <div key={run.id} className="flex items-baseline gap-2 text-sm">
                <span className="min-w-0 flex-1 truncate">{text.what}{run.message ? <span className="text-muted-foreground"> · {run.message}</span> : null}</span>
                <span className="shrink-0 text-muted-foreground">{text.state} · <RelativeTime date={new Date(run.finished_at ?? run.created_at)} /></span>
              </div>
            );
          })}
        </div>
      ) : null}
    </div>
  );
}

function CopyItem({ copy }: { copy: VolumeCopy }) {
  const role = ROLE[copy.role];
  return (
    <Item variant="outline" role="listitem">
      <ItemContent>
        <ItemTitle>{copy.label}<Badge variant={role.variant}>{role.text}</Badge></ItemTitle>
        <ItemDescription>{copy.online ? `on ${copy.server}` : `on ${copy.server}, offline`}</ItemDescription>
      </ItemContent>
    </Item>
  );
}

/** A run that names a Server: the Servers it can name, picked, then started. Hidden when nothing can run. */
function ServerAction({ title, description, verb, offer, busy, onRun }: {
  title: string; description: string; verb: string; offer: Offer; busy: boolean; onRun: (server: string) => void;
}) {
  const [picked, setPicked] = useState("");
  if (offer === null) return null;
  const server = offer.servers.includes(picked) ? picked : offer.servers.length === 1 ? offer.servers[0] ?? "" : "";
  return (
    <Field orientation="responsive">
      <FieldContent>
        <FieldTitle>{title}</FieldTitle>
        <FieldDescription>{description}</FieldDescription>
      </FieldContent>
      <div className="flex gap-2">
        <Select value={server} onValueChange={(value) => setPicked(value ?? "")}>
          <SelectTrigger className="w-40" aria-label={`${title} server`}><SelectValue placeholder="Select a server" /></SelectTrigger>
          <SelectContent>
            <SelectGroup>
              {offer.servers.map((name) => <SelectItem key={name} value={name} label={name}>{name}</SelectItem>)}
            </SelectGroup>
          </SelectContent>
        </Select>
        <Button variant="outline" disabled={busy || server === ""} onClick={() => onRun(server)}>{verb}</Button>
      </div>
    </Field>
  );
}

function RunAction({ title, description, verb, run, busy, onRun }: {
  title: string; description: string; verb: string; run: VolumeRunRequest; busy: boolean; onRun: (run: VolumeRunRequest) => void;
}) {
  return (
    <Field orientation="responsive">
      <FieldContent>
        <FieldTitle>{title}</FieldTitle>
        <FieldDescription>{description}</FieldDescription>
      </FieldContent>
      <Button variant="outline" disabled={busy} onClick={() => onRun(run)}>{verb}</Button>
    </Field>
  );
}
