import type { ReactNode } from "react";
import { MoreVerticalIcon } from "lucide-react";
import type { EnvironmentRef, MachineId } from "@ployz/sdk";
import { InfoHint } from "#/components/info-hint";
import { Alert, AlertAction, AlertDescription, AlertTitle } from "#/components/ui/alert";
import { Button } from "#/components/ui/button";
import { DropdownMenu, DropdownMenuContent, DropdownMenuGroup, DropdownMenuItem, DropdownMenuTrigger } from "#/components/ui/dropdown-menu";
import { Empty, EmptyDescription } from "#/components/ui/empty";
import { Item, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemTitle } from "#/components/ui/item";
import { Spinner } from "#/components/ui/spinner";
import { cn } from "#/lib/utils";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import {
  appliedMirrors, detachedName, editPlacement, failover, lossTime, observePlacement, type Placement, type PlacementEdit,
  type PlacementEntry, placementEntries, type PlacementRow, placementTargets, type PlacementVolume, targetPlacement,
} from "#/modules/volume-placement/volume-placement";
import { useStagePlacement } from "#/modules/volume-placement/volume-placement.hooks";
import { useVolumePlacements } from "#/modules/volume-placement/volume-placement.queries";
import { runWords } from "#/modules/volume-run/volume-copies";
import { copyName, type Member, type VolumeRunView } from "#/modules/volume-run/volume-run";
import { useActiveVolumeRuns, useVolumeMembers } from "#/modules/volume-run/volume-run.queries";
import { SettingsSection } from "#/routes/_protected/cloud/$organizationSlug/-components/SettingsSection";

type StoreVolumePlacementProps = { organizationSlug: string; environment: EnvironmentRef; volume: PlacementVolume };

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

const ROLE_WORDS = { primary: null, mirror: "Mirror", detached: "Detached" } as const satisfies Record<PlacementEntry["role"], string | null>;

export function VolumePlacementPanel({ volume, members, row, running, accepting, disabled, stage }: {
  volume: PlacementVolume;
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
  if (target === null) {
    return <SettingsSection id="placement" title="Placement">
      <Empty variant="placeholder"><EmptyDescription>No server reports a copy.</EmptyDescription></Empty>
    </SettingsSection>;
  }
  const deploying = row?.deploymentId != null;
  const locked = disabled || deploying;
  const applied = appliedMirrors(observed, row);
  const edit = (change: PlacementEdit) => stage(editPlacement(target, change), applied);
  const entries = placementEntries(volume, members, row);
  const targets = placementTargets(volume, members, row, accepting);
  const moveTo = targets.move.filter((server) => server.id !== target.primary);
  const mirrorOn = target.mirrors.length > 0 ? [] : targets.mirror.filter((server) => server.id !== target.primary);
  const offer = failover(observed, row);
  const nameOf = (entry: PlacementEntry) =>
    entry.role === "primary" ? volume.name : entry.role === "mirror" ? copyName(volume.name, entry.server.name) : detachedName(volume);

  const menuOf = (entry: PlacementEntry, name: string): ReactNode => {
    if (entry.change !== null) return null;
    if (entry.role === "primary") {
      return moveTo.length === 0 ? null : <Menu label={name} disabled={locked}>
        {moveTo.map((server) => <DropdownMenuItem key={server.id} onClick={() => edit({ kind: "move", to: server.id })}>Move to {server.name}</DropdownMenuItem>)}
      </Menu>;
    }
    const kind = entry.role === "mirror" ? "remove_mirror" : "delete_copy";
    return <Menu label={name} disabled={locked}>
      <DropdownMenuItem variant="destructive" onClick={() => edit({ kind, server: entry.server.id })}>{entry.role === "mirror" ? "Remove mirror" : "Delete"}</DropdownMenuItem>
    </Menu>;
  };

  return <SettingsSection id="placement" title="Placement" action={mirrorOn.length > 0 ? <DropdownMenu>
    <DropdownMenuTrigger render={<Button variant="outline" size="sm" disabled={locked} />}>Add mirror</DropdownMenuTrigger>
    <DropdownMenuContent align="end"><DropdownMenuGroup>
      {mirrorOn.map((server) => <DropdownMenuItem key={server.id} onClick={() => edit({ kind: "add_mirror", server: server.id })}>{server.name}</DropdownMenuItem>)}
    </DropdownMenuGroup></DropdownMenuContent>
  </DropdownMenu> : null}>
    {offer ? <Alert variant="destructive">
      <AlertTitle>{offer.down.name} is not answering</AlertTitle>
      <AlertDescription>{offer.lostAfter ? `Changes after ${lossTime(offer.lostAfter)} lost` : "Recent changes lost"}</AlertDescription>
      <AlertAction><Button variant="outline" size="sm" disabled={locked} onClick={() => edit({ kind: "move", to: offer.mirror.id })}>Use mirror</Button></AlertAction>
    </Alert> : null}
    {running || deploying ? <div role="status" className="flex items-center gap-2 text-sm text-muted-foreground">
      <Spinner role="presentation" aria-hidden="true" />
      <span>{running ? [runWords(running), running.message].filter(Boolean).join(" · ") : "Deploying"}</span>
    </div> : null}
    <ItemGroup role="list" aria-label="Placement">
      {entries.map((entry) => {
        const name = nameOf(entry);
        const role = ROLE_WORDS[entry.role];
        const struck = entry.change === "remove";
        return <Item key={entry.key} role="listitem" size="xs" data-changed={entry.change === null || deploying ? undefined : true}>
          <ItemContent className="min-w-0">
            <ItemTitle className={cn(struck && "text-muted-foreground line-through")}>{name}</ItemTitle>
            <ItemDescription className="flex items-center gap-1">
              {[entry.server.name, role].filter(Boolean).join(" · ")}
              {entry.role === "mirror" && !struck ? <span className="-my-1 inline-flex">
                <InfoHint label="When it refreshes">Refreshes on every deploy and every 15 minutes.</InfoHint>
              </span> : null}
            </ItemDescription>
          </ItemContent>
          <ItemActions>
            {menuOf(entry, name)}
          </ItemActions>
        </Item>;
      })}
    </ItemGroup>
  </SettingsSection>;
}

function Menu({ label, disabled, children }: { label: string; disabled: boolean; children: ReactNode }) {
  return <DropdownMenu>
    <DropdownMenuTrigger render={<Button variant="ghost" size="icon-sm" aria-label={`Actions for ${label}`} disabled={disabled} />}><MoreVerticalIcon /></DropdownMenuTrigger>
    <DropdownMenuContent align="end" className="w-auto"><DropdownMenuGroup>{children}</DropdownMenuGroup></DropdownMenuContent>
  </DropdownMenu>;
}
