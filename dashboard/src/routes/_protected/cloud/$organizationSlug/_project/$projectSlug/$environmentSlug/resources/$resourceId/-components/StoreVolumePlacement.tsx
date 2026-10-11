import type { ReactNode } from "react";
import { PlusIcon, XIcon } from "lucide-react";
import type { ChangeKind, EnvironmentRef, MachineId } from "@ployz/sdk";
import { InfoHint } from "#/components/info-hint";
import { RelativeTime } from "#/components/relative-time";
import { Alert, AlertAction, AlertDescription, AlertTitle } from "#/components/ui/alert";
import { Button } from "#/components/ui/button";
import { DropdownMenu, DropdownMenuContent, DropdownMenuGroup, DropdownMenuItem, DropdownMenuTrigger } from "#/components/ui/dropdown-menu";
import { Empty, EmptyDescription } from "#/components/ui/empty";
import { Field, FieldContent, FieldDescription, FieldLabel } from "#/components/ui/field";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "#/components/ui/select";
import { Spinner } from "#/components/ui/spinner";
import { cn } from "#/lib/utils";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import {
  appliedMirrors, editPlacement, failover, lossTime, observePlacement, type Placement, type PlacementEdit,
  placementChanges, type PlacementItem, placementForm, type PlacementRow, placementTargets, type PlacementVolume, targetPlacement,
  undoPlacement,
} from "#/modules/volume-placement/volume-placement";
import { useStagePlacement } from "#/modules/volume-placement/volume-placement.hooks";
import { useVolumePlacements } from "#/modules/volume-placement/volume-placement.queries";
import { runWords } from "#/modules/volume-run/volume-copies";
import type { Member, VolumeRunView } from "#/modules/volume-run/volume-run";
import { useActiveVolumeRuns, useVolumeMembers } from "#/modules/volume-run/volume-run.queries";
import { SettingsSection } from "#/routes/_protected/cloud/$organizationSlug/-components/SettingsSection";

/** A Volume as the panel shows it: who mounts it names who its Primary runs. */
type PanelVolume = PlacementVolume & { readonly mounts: readonly { readonly service: string }[] };

type StoreVolumePlacementProps = { organizationSlug: string; environment: EnvironmentRef; volume: PanelVolume };

/** A deployed managed Volume's Primary and Mirrors. Every edit is staged; the next Deploy runs it before its Services. */
export function StoreVolumePlacement({ organizationSlug, environment, volume }: StoreVolumePlacementProps) {
  const lens = useRuntimeLens(organizationSlug);
  const members = useVolumeMembers(organizationSlug, environment, volume.id).data;
  const row = useVolumePlacements(organizationSlug).data?.find((entry) => entry.volumeId === volume.id) ?? null;
  const running = useActiveVolumeRuns(organizationSlug).data?.find((run) => run.volume_id === volume.id) ?? null;
  const { stage, pending } = useStagePlacement(organizationSlug, environment, volume);
  if (members === undefined) {
    return <SettingsSection id="placement" title="Placement">
      <Empty variant="placeholder"><EmptyDescription>Waiting for your servers.</EmptyDescription></Empty>
    </SettingsSection>;
  }
  const accepting = new Set(lens.machines.filter((machine) => machine.acceptsServices).map((machine) => machine.id));
  return <VolumePlacementPanel volume={volume} members={members} row={row} running={running} disabled={pending}
    accepting={members.filter((member) => accepting.has(member.machine.id)).map((member) => member.machine.id)} stage={stage} />;
}

/** A staged chip or select takes its kind's colour, the same three Details uses. */
const KIND_CLASSES = {
  add: "border-success-border bg-success-soft text-success",
  update: "border-changed-border bg-changed-soft text-changed-deep",
  remove: "border-destructive-border bg-destructive-soft text-destructive",
} satisfies Record<ChangeKind, string>;

export function VolumePlacementPanel({ volume, members, row, running, accepting, disabled, stage }: {
  volume: PanelVolume;
  members: readonly Member[];
  row: PlacementRow | null;
  running: VolumeRunView | null;
  /** Servers that accept Services: the only ones a move or a mirror may name. */
  accepting: readonly MachineId[];
  disabled: boolean;
  stage: (staged: Placement | null, mirrors: readonly MachineId[]) => void;
}) {
  const observed = observePlacement(members);
  const target = targetPlacement(observed, row);
  const form = placementForm(volume, members, row);
  if (target === null || form === null) {
    return <SettingsSection id="placement" title="Placement">
      <Empty variant="placeholder"><EmptyDescription>No server reports a copy.</EmptyDescription></Empty>
    </SettingsSection>;
  }
  const deploying = row?.deploymentId != null;
  const locked = disabled || deploying;
  const applied = appliedMirrors(observed, row);
  const edit = (change: PlacementEdit) => stage(editPlacement(target, change), applied);
  const targets = placementTargets(volume, members, row, accepting);
  // A staged move's select offers the live Primary back: picking it drops the move, like typing a field back.
  const staged = row ? placementChanges(volume, members, row).find((change) => change.kind === "move") ?? null : null;
  const back = staged?.from ?? null;
  const moveTo = [...(back ? [back] : []), ...targets.move.filter((server) => server.id !== target.primary && server.id !== back?.id)];
  const mirrorOn = target.mirrors.length > 0 ? [] : targets.mirror.filter((server) => server.id !== target.primary);
  const offer = failover(observed, row);
  // While a Deploy runs the change, the form shows where it goes without the staged colours.
  const kindOf = (item: PlacementItem) => (deploying ? null : item.change);
  const runs = [...new Set(volume.mounts.map((mount) => mount.service))];
  const primaryChoices = [form.primary.server, ...moveTo].map((server) => ({ value: server.id, label: server.name }));

  return <SettingsSection id="placement" title="Placement">
    {offer ? <Alert variant="destructive">
      <AlertTitle>{offer.down.name} is not answering</AlertTitle>
      <AlertDescription>{offer.lostAfter ? `Changes after ${lossTime(offer.lostAfter)} lost` : "Recent changes lost"}</AlertDescription>
      <AlertAction><Button variant="outline" size="sm" disabled={locked} onClick={() => edit({ kind: "move", to: offer.mirror.id })}>Use mirror</Button></AlertAction>
    </Alert> : null}
    {running || deploying ? <div role="status" className="flex items-center gap-2 text-sm text-muted-foreground">
      <Spinner role="presentation" aria-hidden="true" />
      <span>{running ? [runWords(running), running.message].filter(Boolean).join(" · ") : "Deploying"}</span>
    </div> : null}
    <Field orientation="responsive">
      <FieldContent>
        <FieldLabel>Primary</FieldLabel>
        {runs.length > 0 ? <FieldDescription>Runs {runs.join(", ")}</FieldDescription> : null}
      </FieldContent>
      <Select items={primaryChoices} value={form.primary.server.id} disabled={locked || moveTo.length === 0}
        onValueChange={(next) => {
          if (next === null || next === target.primary) return;
          if (row && staged && next === back?.id) stage(undoPlacement(volume, members, row, staged), applied);
          else edit({ kind: "move", to: next });
        }}>
        <SelectTrigger aria-label="Primary" className="w-full @md/field-group:w-56" data-changed={kindOf(form.primary) ? true : undefined}>
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          {primaryChoices.map((choice) => <SelectItem key={choice.value} value={choice.value}>{choice.label}</SelectItem>)}
        </SelectContent>
      </Select>
    </Field>
    <Field orientation="responsive">
      <FieldContent>
        <FieldLabel className="items-center gap-1">
          Mirrors
          <span className="-my-1 inline-flex"><InfoHint label="When it refreshes">Refreshes on every deploy and every 15 minutes.</InfoHint></span>
        </FieldLabel>
      </FieldContent>
      <Chips label="Mirrors">
        {form.mirrors.map((item) => <Chip key={item.server.id} item={item} kind={kindOf(item)}
          note={item.newest ? <RelativeTime date={item.newest} /> : null}
          action={<Button variant="ghost" size="icon-xs" aria-label={`Remove mirror on ${item.server.name}`} disabled={locked}
            onClick={() => edit({ kind: "remove_mirror", server: item.server.id })}><XIcon /></Button>} />)}
        {mirrorOn.length > 0 ? <DropdownMenu>
          <DropdownMenuTrigger render={<Button variant="outline" size="xs" aria-label="Add mirror" disabled={locked}
            className="h-7 rounded-full border-dashed px-2.5 text-sm" />}><PlusIcon />Add</DropdownMenuTrigger>
          <DropdownMenuContent align="start"><DropdownMenuGroup>
            {mirrorOn.map((server) => <DropdownMenuItem key={server.id} onClick={() => edit({ kind: "add_mirror", server: server.id })}>{server.name}</DropdownMenuItem>)}
          </DropdownMenuGroup></DropdownMenuContent>
        </DropdownMenu> : form.mirrors.length === 0 ? <span className="text-sm text-muted-foreground">None</span> : null}
      </Chips>
    </Field>
    {form.detached.length > 0 ? <Field orientation="responsive">
      <FieldContent><FieldLabel>Detached</FieldLabel></FieldContent>
      <Chips label="Detached">
        {form.detached.map((item) => <Chip key={item.server.id} item={item} kind={kindOf(item)} note={null}
          action={<Button variant="ghost" size="icon-xs" aria-label={`Delete copy on ${item.server.name}`} disabled={locked}
            onClick={() => edit({ kind: "delete_copy", server: item.server.id })}><XIcon /></Button>} />)}
      </Chips>
    </Field> : null}
  </SettingsSection>;
}

function Chips({ label, children }: { label: string; children: ReactNode }) {
  return <div role="list" aria-label={label} className="flex min-w-0 flex-wrap items-center gap-1.5 @md/field-group:w-56">{children}</div>;
}

/**
 * One Server in a group. A live chip says when its copy refreshed and carries its action; a staged one wears its kind's
 * colour, "New" when the Deploy creates it, struck when it removes it, and no action: Details discards.
 */
function Chip({ item, kind, note, action }: { item: PlacementItem; kind: ChangeKind | null; note: ReactNode; action: ReactNode }) {
  return <span role="listitem" data-change={kind ?? undefined}
    className={cn("inline-flex h-7 items-center gap-1.5 rounded-full border bg-background pr-1 pl-2.5 text-sm", kind && KIND_CLASSES[kind], item.change !== null && "pr-2.5")}>
    <span className={cn(item.change === "remove" && "line-through")}>{item.server.name}</span>
    {kind === "add" ? <span className="text-xs">New</span>
      : item.change === null && note ? <span className="text-xs text-muted-foreground">{note}</span> : null}
    {item.change === null ? action : null}
  </span>;
}
