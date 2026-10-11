import type { ReactNode } from "react";
import { Link, useLoaderData } from "@tanstack/react-router";
import { Trash2Icon } from "lucide-react";
import type { EnvironmentRef, MachineId } from "@ployz/sdk";
import { RelativeTime } from "#/components/relative-time";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Empty, EmptyDescription } from "#/components/ui/empty";
import { Field, FieldContent, FieldDescription, FieldTitle } from "#/components/ui/field";
import { cn } from "#/lib/utils";
import { requireView, useStoreViews, volumesQuery } from "#/modules/config-store/store-view.queries";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import { mirrorPanel, type MirrorPanel, type Placement, type PlacementVolume } from "#/modules/volume-placement/volume-placement";
import { useStagePlacement } from "#/modules/volume-placement/volume-placement.hooks";
import { useVolumePlacements } from "#/modules/volume-placement/volume-placement.queries";
import { copyName } from "#/modules/volume-run/volume-run";
import { useVolumeMembers } from "#/modules/volume-run/volume-run.queries";
import { DangerRow } from "#/routes/_protected/cloud/$organizationSlug/-components/danger-row";
import { SettingsSection } from "#/routes/_protected/cloud/$organizationSlug/-components/SettingsSection";
import { CanvasInspectorHeader } from "../../../-components/CanvasInspectorHeader";
import { CanvasInspectorNotFound } from "../../../-components/CanvasInspectorRouteStates";
import { mirrorChip } from "../../../-components/canvas/MirrorNode";
import { ENVIRONMENT_RESOURCE_ROUTE_TO, ENVIRONMENT_ROUTE_FROM, type EnvironmentRouteParams } from "../../../-components/environment-route-paths";
import { MIRROR_REFRESHES } from "./StoreVolumePlacement";

/** A Volume's mirror as a resource of its own: where its copy is, how fresh, and the two things it can become. */
export function StoreMirrorDrawer({ params, volumeId, server }: { params: EnvironmentRouteParams; volumeId: string; server: MachineId }) {
  const { store } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const [volumes] = useStoreViews(params.organizationSlug, [volumesQuery(store)] as const);
  const volume = requireView(volumes).volumes.find((candidate) => candidate.id === volumeId);
  if (volume === undefined) return <CanvasInspectorNotFound noun="Mirror" />;
  return <MirrorDrawerData params={params} environment={store} volume={volume} server={server} />;
}

function MirrorDrawerData({ params, environment, volume, server }: {
  params: EnvironmentRouteParams;
  environment: EnvironmentRef;
  volume: PlacementVolume;
  server: MachineId;
}) {
  const { organizationSlug } = params;
  const lens = useRuntimeLens(organizationSlug);
  const members = useVolumeMembers(organizationSlug, environment, volume.id).data;
  const row = useVolumePlacements(organizationSlug).data?.find((entry) => entry.volumeId === volume.id) ?? null;
  const { stage, pending } = useStagePlacement(organizationSlug, environment, volume);
  if (members === undefined) {
    return <Empty variant="placeholder"><EmptyDescription>Waiting for your servers.</EmptyDescription></Empty>;
  }
  const accepting = new Set(lens.machines.filter((machine) => machine.acceptsServices).map((machine) => machine.id));
  const panel = mirrorPanel(volume, members, row, server,
    members.filter((member) => accepting.has(member.machine.id)).map((member) => member.machine.id));
  // A stale link, or the mirror went with a Deploy or a discard while open.
  if (panel === null) return <CanvasInspectorNotFound noun="Mirror" />;
  return <MirrorDrawerView params={params} panel={panel} disabled={pending} stage={stage} />;
}

/**
 * One mirror's panel. A live mirror can become the Primary or go; a staged one wears its kind and only Details discards
 * it. Both actions stage the same placement the Volume's own panel would.
 */
export function MirrorDrawerView({ params, panel, disabled, stage }: {
  params: EnvironmentRouteParams;
  panel: MirrorPanel;
  disabled: boolean;
  stage: (staged: Placement | null, mirrors: readonly MachineId[]) => void;
}) {
  const { volume, server, change, newest } = panel;
  const name = copyName(volume.name, server.name);
  const chip = mirrorChip(change);
  const volumeLink = (children: ReactNode) => (
    <Link to={ENVIRONMENT_RESOURCE_ROUTE_TO} params={{ ...params, resourceId: volume.id }} className="text-foreground underline-offset-4 hover:underline">
      {children}
    </Link>
  );

  return (
    <div className="flex h-full min-h-0 flex-col">
      <CanvasInspectorHeader params={params}>
        <p className="flex min-w-0 items-center gap-2 font-semibold">
          <span className={cn("truncate", change === "remove" && "line-through")}>{name}</span>
          {chip ? <Badge variant={chip.variant}>{chip.label}</Badge> : null}
        </p>
        <p className="truncate text-sm text-muted-foreground">Mirror of {volumeLink(volume.name)}</p>
      </CanvasInspectorHeader>
      <div className="min-h-0 flex-1 overflow-y-auto px-4 pt-4 pb-8">
        <div className="mx-auto flex w-full max-w-2xl flex-col gap-6">
          <SettingsSection id="mirror" title="Mirror">
            <Row title="Server">{server.name}</Row>
            <Row title="Last refresh">{newest && change !== "add" ? <RelativeTime date={newest} /> : "Not yet"}</Row>
            <Row title="Refreshes">{MIRROR_REFRESHES}</Row>
            <Row title="Restore points">{volumeLink(`In ${volume.name}`)}</Row>
            {panel.promote ? (
              <Field orientation="responsive">
                <FieldContent>
                  <FieldTitle>Make primary</FieldTitle>
                  <FieldDescription>Moves {volume.name} and its services to {server.name} on next deploy.</FieldDescription>
                </FieldContent>
                <Button variant="outline" disabled={disabled} onClick={() => stage(panel.promote, panel.applied)}>Make primary</Button>
              </Field>
            ) : null}
          </SettingsSection>
          {panel.remove ? (
            <SettingsSection id="danger" title="Danger">
              <DangerRow title="Remove mirror" description={`Its copy on ${server.name} is deleted on next deploy.`}
                action={(
                  <Button variant="destructive" disabled={disabled} onClick={() => stage(panel.remove, panel.applied)}>
                    <Trash2Icon data-icon="inline-start" />
                    Remove mirror
                  </Button>
                )} />
            </SettingsSection>
          ) : null}
        </div>
      </div>
    </div>
  );
}

function Row({ title, children }: { title: string; children: ReactNode }) {
  return (
    <Field orientation="responsive">
      <FieldContent><FieldTitle>{title}</FieldTitle></FieldContent>
      <span className="text-sm">{children}</span>
    </Field>
  );
}
