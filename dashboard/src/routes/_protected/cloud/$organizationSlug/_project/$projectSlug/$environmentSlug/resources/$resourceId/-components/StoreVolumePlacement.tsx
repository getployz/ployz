import type { ReactNode } from "react";
import { EllipsisIcon } from "lucide-react";
import type { EnvironmentRef, MachineId } from "@ployz/sdk";
import { Alert, AlertDescription } from "#/components/ui/alert";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { DropdownMenu, DropdownMenuContent, DropdownMenuGroup, DropdownMenuItem, DropdownMenuTrigger } from "#/components/ui/dropdown-menu";
import { Empty, EmptyDescription } from "#/components/ui/empty";
import { Item, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemTitle } from "#/components/ui/item";
import { Spinner } from "#/components/ui/spinner";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import {
  appliedMirrors, detachedCopies, detachedName, editPlacement, failover, lossTime, observePlacement, type Placement, placementChanges,
  type PlacementEdit, type PlacementRow, placementTargets, type PlacementVolume, placementWords, targetPlacement,
} from "#/modules/volume-placement/volume-placement";
import { useStagePlacement } from "#/modules/volume-placement/volume-placement.hooks";
import { useVolumePlacements } from "#/modules/volume-placement/volume-placement.queries";
import { runWords } from "#/modules/volume-run/volume-copies";
import { copyName, type Member, type VolumeRunView } from "#/modules/volume-run/volume-run";
import { useActiveVolumeRuns, useVolumeMembers } from "#/modules/volume-run/volume-run.queries";
import { RowWarning } from "#/routes/_protected/cloud/$organizationSlug/-components/SettingsSection";

type StoreVolumePlacementProps = { organizationSlug: string; environment: EnvironmentRef; volume: PlacementVolume };

/** A deployed managed Volume's Primary and Mirrors. Every edit is staged; the next Deploy runs it before its Services. */
export function StoreVolumePlacement({ organizationSlug, environment, volume }: StoreVolumePlacementProps) {
  const lens = useRuntimeLens(organizationSlug);
  const members = useVolumeMembers(organizationSlug, environment, volume.id).data;
  const row = useVolumePlacements(organizationSlug).data?.find((entry) => entry.volumeId === volume.id) ?? null;
  const running = useActiveVolumeRuns(organizationSlug).data?.find((run) => run.volume_id === volume.id) ?? null;
  const { stage, pending } = useStagePlacement(organizationSlug, environment, volume);
  if (members === undefined) return <Empty variant="placeholder"><EmptyDescription>Waiting for your servers.</EmptyDescription></Empty>;
  const accepting = new Set(lens.machines.filter((machine) => machine.acceptsServices).map((machine) => machine.id));
  return <VolumePlacementPanel volume={volume} members={members} row={row} running={running} disabled={pending}
    accepting={members.filter((member) => accepting.has(member.machine.id)).map((member) => member.machine.id)} stage={stage} />;
}

export function VolumePlacementPanel({ volume, members, row, running, accepting, disabled, stage }: {
  volume: PlacementVolume;
  members: readonly Member[];
  row: PlacementRow | null;
  running: VolumeRunView | null;
  /** Servers that accept Services: the only ones a move or a mirror may name. */
  accepting: readonly MachineId[];
  disabled: boolean;
  stage: (staged: Placement, mirrors: readonly MachineId[]) => void;
}) {
  const observed = observePlacement(members);
  const target = targetPlacement(observed, row);
  if (target === null) return <Empty variant="placeholder"><EmptyDescription>No server reports a copy.</EmptyDescription></Empty>;
  const name = (id: MachineId) => members.find((member) => member.machine.id === id)?.machine.name ?? id;
  const deploying = row?.deploymentId != null;
  const locked = disabled || deploying;
  const edit = (change: PlacementEdit) => stage(editPlacement(target, change), appliedMirrors(observed, row));
  const applied = appliedMirrors(observed, row);
  const targets = placementTargets(volume, members, row, accepting);
  const moveTo = targets.move.filter((server) => server.id !== target.primary);
  const mirrorOn = targets.mirror.filter((server) => server.id !== target.primary);
  const detached = detachedCopies(observed, row).filter((copy) => !target.deletes.includes(copy.server.id) && copy.server.id !== target.primary);
  const changes = placementChanges(volume, members, row);
  const offer = failover(observed, row);
  const moved = changes.some((change) => change.kind === "move") || detached.length > 0;

  return <div className="flex flex-col gap-4">
    {offer ? <Alert variant="destructive">
      <AlertDescription className="flex flex-wrap items-center gap-x-1.5">
        <span>{offer.down.name} is not answering ·</span>
        <Button variant="link" className="h-auto p-0" disabled={locked} onClick={() => edit({ kind: "move", to: offer.mirror.id })}>Use mirror on {offer.mirror.name}</Button>
        {offer.lostAfter ? <span>· Changes after {lossTime(offer.lostAfter)} lost</span> : null}
      </AlertDescription>
    </Alert> : null}
    {running ? <div role="status" className="flex items-center gap-2 text-sm">
      <Spinner role="presentation" aria-hidden="true" /><span>{runWords(running)}{running.message ? ` · ${running.message}` : ""}</span>
    </div> : null}
    <ItemGroup role="list" aria-label="Placement">
      <Item role="listitem">
        <ItemContent>
          <ItemTitle>{volume.name}<Badge variant="secondary">Primary</Badge>{target.primary !== observed.primary?.id ? <Badge variant="info">Staged</Badge> : null}</ItemTitle>
          <ItemDescription>{name(target.primary)}</ItemDescription>
        </ItemContent>
        {moveTo.length > 0 ? <ItemActions><Menu label={`${volume.name} options`} disabled={locked}>
          {moveTo.map((server) => <DropdownMenuItem key={server.id} onClick={() => edit({ kind: "move", to: server.id })}>Move to {server.name}</DropdownMenuItem>)}
        </Menu></ItemActions> : null}
      </Item>
      {target.mirrors.map((id) => <Item key={id} role="listitem">
        <ItemContent>
          <ItemTitle>{copyName(volume.name, name(id))}<Badge variant="info">Mirror</Badge>{applied.includes(id) ? null : <Badge variant="info">Staged</Badge>}</ItemTitle>
          <ItemDescription>{name(id)} · Refreshes on every deploy and every 15 minutes</ItemDescription>
        </ItemContent>
        <ItemActions><Menu label={`${copyName(volume.name, name(id))} options`} disabled={locked}>
          <DropdownMenuItem variant="destructive" onClick={() => edit({ kind: "remove_mirror", server: id })}>Remove mirror</DropdownMenuItem>
        </Menu></ItemActions>
      </Item>)}
      {detached.map((copy) => <Item key={copy.server.id} role="listitem">
        <ItemContent>
          <ItemTitle>{detachedName(volume)}<Badge variant="outline">Detached</Badge></ItemTitle>
          <ItemDescription>{copy.server.name} · Kept until you delete it</ItemDescription>
        </ItemContent>
        <ItemActions><Menu label={`${detachedName(volume)} options`} disabled={locked}>
          <DropdownMenuItem variant="destructive" onClick={() => edit({ kind: "delete_copy", server: copy.server.id })}>Delete</DropdownMenuItem>
        </Menu></ItemActions>
      </Item>)}
    </ItemGroup>
    {target.mirrors.length > 0 ? <p className="text-sm text-muted-foreground">One mirror for now</p>
      : mirrorOn.length > 0 ? <AddMirror warn={moved} disabled={locked} servers={mirrorOn} add={(id) => edit({ kind: "add_mirror", server: id })} />
      : moved ? <RowWarning>No mirror</RowWarning> : null}
    {changes.length > 0 ? <div className="flex flex-col gap-1 text-sm">
      {changes.map(placementWords).map((words) => <p key={words.title}><span className="font-medium">{words.title}</span> <span className="text-muted-foreground">{words.detail}</span></p>)}
      <p className="text-muted-foreground">{deploying ? "Deploying" : "Staged for the next deploy"}</p>
    </div> : null}
  </div>;
}

function Menu({ label, disabled, children }: { label: string; disabled: boolean; children: ReactNode }) {
  return <DropdownMenu>
    <DropdownMenuTrigger render={<Button variant="ghost" size="icon-xs" aria-label={label} disabled={disabled} />}><EllipsisIcon /></DropdownMenuTrigger>
    <DropdownMenuContent align="end"><DropdownMenuGroup>{children}</DropdownMenuGroup></DropdownMenuContent>
  </DropdownMenu>;
}

function AddMirror({ warn, disabled, servers, add }: {
  warn: boolean; disabled: boolean; servers: readonly { readonly id: MachineId; readonly name: string }[]; add: (id: MachineId) => void;
}) {
  const button = <DropdownMenu>
    <DropdownMenuTrigger render={<Button variant="outline" size="sm" disabled={disabled} />}>Add mirror</DropdownMenuTrigger>
    <DropdownMenuContent align="start"><DropdownMenuGroup>
      {servers.map((server) => <DropdownMenuItem key={server.id} onClick={() => add(server.id)}>{server.name}</DropdownMenuItem>)}
    </DropdownMenuGroup></DropdownMenuContent>
  </DropdownMenu>;
  return warn ? <RowWarning action={button}>No mirror</RowWarning> : <div>{button}</div>;
}
