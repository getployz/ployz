import { StoreVolumeMounts } from "#/modules/config-store/StoreVolumeMounts";
import { useState } from "react";
import { Schema } from "effect";
import { useLoaderData, useNavigate, useSearch } from "@tanstack/react-router";
import { Trash2Icon } from "lucide-react";
import type { DiffView, EnvironmentRef, EnvironmentView, MachineId, ServiceListing, VolumeListing } from "@ployz/sdk";
import { Button } from "#/components/ui/button";
import { Empty, EmptyDescription } from "#/components/ui/empty";
import { Field, FieldContent, FieldDescription, FieldError, FieldLabel, FieldTitle } from "#/components/ui/field";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "#/components/ui/tabs";
import { diffQuery, environmentSettingsQuery, requireView, servicesQuery, useStoreViews, volumesQuery } from "#/modules/config-store/store-view.queries";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { DEFAULT_VOLUME_GB, effectiveVolumes, gigabytes, volumeStorage, volumeStorageText } from "#/modules/config-store/store-volumes";
import { VolumeAdvanced, VolumeStorageFields } from "#/modules/config-store/VolumeStorageFields";
import { CanvasInspectorHeader } from "../../../-components/CanvasInspectorHeader";
import { replicaCount } from "#/modules/config-store/volume-sharing";
import { StoreRefused } from "#/modules/config-store/store.contract";
import { InfoHint } from "#/components/info-hint";
import { Switch } from "#/components/ui/switch";
import { serviceSettingRows } from "#/modules/config-store/store-services";
import { SettingsSection, SHARED_VOLUME_WHY } from "#/routes/_protected/cloud/$organizationSlug/-components/SettingsSection";
import { SafetyLine, useVolumeCopies, VolumeCopiesPanel } from "./StoreVolumeCopies";
import { mirrorResourceId } from "#/modules/volume-placement/volume-placement";
import { VOLUME_PAGES, volumePageSchema } from "./volume-pages";
import { DangerRow } from "#/routes/_protected/cloud/$organizationSlug/-components/danger-row";
import { ServiceSettingInput } from "../../../services/$serviceId/-components/ServiceSettingInput";
import { CanvasInspectorNotFound } from "../../../-components/CanvasInspectorRouteStates";
import { CanvasInspectorNameEditor } from "../../../-components/CanvasInspectorNameEditor";
import { changedProps, dnsLabelError } from "#/modules/config-store/store-services";
import { useStoreChangeActions } from "../../../-components/canvas/useStoreChangeActions";
import { DEPLOYMENT_PAGE_ROUTE_TO } from "../../../-components/deployment-page";
import { ENVIRONMENT_INDEX_ROUTE_TO, ENVIRONMENT_RESOURCE_ROUTE_TO, ENVIRONMENT_ROUTE_FROM } from "../../../-components/environment-route-paths";

type StoreVolume = { organizationSlug: string; environment: EnvironmentRef; volume: VolumeListing };

const RESOURCE_ROUTE_FROM = "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/resources/$resourceId";

type VolumeResourceRouteParams = { organizationSlug: string; projectSlug: string; environmentSlug: string; resourceId: string };

/**
 * A Volume in the Config Store, in two tabs. Copies: the live one and every place it survives a dead Server, with a
 * mirror opening in detail at its own address. Settings: its space, who mounts it, and its removal. Every edit is staged
 * like any other change, except deleting an old copy, which runs at once after a confirm.
 */
export function StoreVolumeDrawer({ params, volumeId = params.resourceId, copy = null }: {
  params: VolumeResourceRouteParams;
  volumeId?: string;
  /** The mirror open in detail, from its own resource address. */
  copy?: MachineId | null;
}) {
  const { store } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const { organizationSlug } = params;
  const views = useStoreViews(organizationSlug, [volumesQuery(store), servicesQuery(store), diffQuery(store), environmentSettingsQuery(store)] as const);
  const listings = requireView(views[0]).volumes;
  const services = requireView(views[1]).services;
  const settings = requireView(views[3]);
  const volumes = effectiveVolumes(listings, services, settings);
  const volume = volumes.find((candidate) => candidate.id === volumeId);
  // A stale link, or removed while open (a new Volume's removal, or from the CLI).
  if (!volume) return <CanvasInspectorNotFound noun="Volume" />;
  return <VolumeDrawer params={params} environment={store} volume={volume} volumes={volumes} services={services}
    settings={settings} diff={requireView(views[2])} copy={copy} />;
}

function VolumeDrawer({ params, environment, volume, volumes, services, settings, diff, copy }: {
  params: VolumeResourceRouteParams;
  environment: EnvironmentRef;
  volume: VolumeListing;
  volumes: readonly VolumeListing[];
  services: readonly ServiceListing[];
  settings: EnvironmentView;
  diff: DiffView;
  copy: MachineId | null;
}) {
  const { organizationSlug } = params;
  const { tab } = useSearch({ from: RESOURCE_ROUTE_FROM });
  const navigate = useNavigate({ from: ENVIRONMENT_RESOURCE_ROUTE_TO });
  const writer = useStoreWriter(organizationSlug);
  const data = useVolumeCopies(organizationSlug, environment, volume);
  // Staged counts count: a shared volume warns before the Deploy that would share it.
  const replicasOf = (service: string) => replicaCount(serviceSettingRows(settings, service).get("replicas"));
  const state: StoreVolume = { organizationSlug, environment, volume };
  const removing = volume.change === "delete";
  const renamed = diff.changes.find((change) => change.type === "volume" && change.id === volume.id)
    ?.settings.find((row) => row.path === "name" || row.path.endsWith(".name"));
  // A DNS label no other Volume here has.
  const nameSchema = Schema.String.check(Schema.makeFilter<string>((name) => dnsLabelError(name)
    ?? (volumes.some((other) => other.id !== volume.id && other.name === name) ? `A volume here is already named ${name}.` : undefined)));
  const managed = volume.deployed && volume.storage.kind === "provisioned" && !removing;
  const mount = volume.mounts[0];
  const open = (next: MachineId | null) => void navigate({
    params: { ...params, resourceId: next === null ? volume.id : mirrorResourceId(volume.id, next) },
    search: (prev) => ({ ...prev, tab: "copies" as const }),
  });

  return (
    <div className="flex h-full min-h-0 flex-col">
      <CanvasInspectorHeader params={params}>
        {removing ? <p className="truncate font-semibold">{volume.name}</p> : (
          <CanvasInspectorNameEditor value={volume.name} schema={nameSchema} editTitle="Edit volume name"
            editDescription="Rename this volume. Its data and mounts stay." placeholder="Volume name"
            {...changedProps(renamed)}
            onRename={(name) => writer.commit({ command: "rename_volume", environment, volume: volume.name, name })} />
        )}
        <p className="truncate text-sm text-muted-foreground">
          {mount ? `${[...new Set(volume.mounts.map((entry) => entry.service))].join(", ")} · ${mount.path}` : "Not mounted"}
        </p>
        {managed && data.copies ? <SafetyLine copies={data.copies} /> : null}
      </CanvasInspectorHeader>
      <div className="flex min-h-0 flex-1 flex-col px-4 pb-4">
        <Tabs value={Schema.is(volumePageSchema)(tab) ? tab : "copies"}
          onValueChange={(value) => {
            if (Schema.is(volumePageSchema)(value)) void navigate({ search: (prev) => ({ ...prev, tab: value }), replace: true });
          }}
          className="flex min-h-0 flex-1 flex-col overflow-visible sm:overflow-clip">
          <TabsList variant="line" className="-mx-4 max-w-none shrink-0 overflow-x-auto sm:mx-0 sm:max-w-full">
            {VOLUME_PAGES.map((page) => <TabsTrigger key={page.id} value={page.id}>{page.label}</TabsTrigger>)}
          </TabsList>
          <TabsContent value="copies" className="mt-3 min-h-0 flex-1 overflow-clip">
            <div className="h-full overflow-y-auto pr-1 pb-8">
              <div className="mx-auto flex w-full max-w-2xl flex-col gap-6">
                {managed ? <VolumeCopiesPanel {...data} open={copy} onOpen={open} /> : (
                  <Empty variant="placeholder">
                    <EmptyDescription>{removing ? "Removed on next deploy." : volume.deployed ? "Managed volumes only." : "Deploy first."}</EmptyDescription>
                  </Empty>
                )}
              </div>
            </div>
          </TabsContent>
          <TabsContent value="settings" className="mt-3 min-h-0 flex-1 overflow-clip">
            <div className="h-full overflow-y-auto pr-1 pb-8">
              <div className="mx-auto flex w-full max-w-2xl flex-col gap-6">
                <SettingsSection id="space" title="Space">
                  <StoreVolumeStorage key={`${volume.id}:${JSON.stringify(volume.storage)}`} state={state} removing={removing} />
                </SettingsSection>
                {removing ? <SettingsSection id="mounts" title="Mounts">
                    <Empty variant="placeholder">
                      <EmptyDescription>Removed on next deploy.</EmptyDescription>
                    </Empty>
                  </SettingsSection> : <StoreVolumeMounts key={volume.id} resourceId={volume.id} organizationSlug={organizationSlug} environment={environment}
                    services={services} volumes={volumes} diff={diff} replicasOf={replicasOf} />}
                <SettingsSection id="danger" title="Danger">
                  <StoreVolumeDanger state={state} params={params} version={diff.version} />
                </SettingsSection>
              </div>
            </div>
          </TabsContent>
        </Tabs>
      </div>
    </div>
  );
}

function StoreVolumeStorage({ state, removing }: { state: StoreVolume; removing: boolean }) {
  const writer = useStoreWriter(state.organizationSlug);
  const { storage, storage_locked } = state.volume;
  const managed = storage.kind === "provisioned";
  const sizeGB = managed ? gigabytes(storage.maximumBytes) : DEFAULT_VOLUME_GB;
  // Autosaved like a service's fields: each edit is one staged command.
  const save = (next: NonNullable<ReturnType<typeof volumeStorage>>) =>
    writer.commit({ command: "set_volume_storage", environment: state.environment, volume: state.volume.name, storage: next });

  if (storage_locked || removing) {
    return (
      <>
      <Field orientation="responsive">
        <FieldContent>
          <FieldTitle>{managed ? "Managed volume" : "Docker volume"}</FieldTitle>
          <FieldDescription>Fixed after the first deploy.</FieldDescription>
        </FieldContent>
        <span className="text-sm">{volumeStorageText(storage)}</span>
      </Field>
      {removing ? null : <VolumeAdvanced><StoreSharedWrites state={state} /></VolumeAdvanced>}
      </>
    );
  }
  return (
    <VolumeStorageFields managed={managed} sizeGB={sizeGB} onSizeChange={() => undefined} advanced={<StoreSharedWrites state={state} />}
      onManagedChange={(next) => save(next ? { kind: "provisioned", maximumBytes: Number(DEFAULT_VOLUME_GB) * 1e9 } : { kind: "docker" })}
      limitInput={(
        <ServiceSettingInput ariaLabel="Storage limit" inputMode="decimal" suffix="GB"
          placeholder={DEFAULT_VOLUME_GB} value={sizeGB} isChanged={false}
          validate={(raw) => volumeStorage(true, raw) ? null : "Enter at least 0.001 GB."}
          onCommit={(raw) => save(volumeStorage(true, raw) ?? storage)} />
      )} />
  );
}

function StoreVolumeDanger({ state, params, version }: { state: StoreVolume; params: VolumeResourceRouteParams; version: string }) {
  const writer = useStoreWriter(state.organizationSlug);
  const navigate = useNavigate();
  // Its data goes with the Deploy that removes it, which asks first; that Deploy opens its page.
  const actions = useStoreChangeActions(state.organizationSlug, state.environment, version,
    (deploymentId) => void navigate({ to: DEPLOYMENT_PAGE_ROUTE_TO, params: { ...params, deploymentId } }));
  const { volume } = state;
  const removing = volume.change === "delete";
  const mounts = volume.mounts.length;

  function remove() {
    // Staged: the canvas marks it Removing, and a Deploy that deletes its data asks first.
    writer.commit({ command: "remove_volume", environment: state.environment, volume: volume.name });
    void navigate({ to: ENVIRONMENT_INDEX_ROUTE_TO, params, search: (prev) => prev });
  }

  return (
    <>
      <DangerRow
        title={removing && volume.deployed ? "Delete its data" : removing ? "Removed on next deploy" : "Delete this volume"}
        description={removing && volume.deployed
          ? "Its files are still on your servers. Deploying deletes them, with this environment's other changes, once you confirm."
          : removing
          ? "Nothing of it is on your servers yet. Keep the volume to undo this."
          : volume.deployed
          ? "Its data on your servers goes with it. Deploy asks you to confirm first."
          : mounts > 0 ? `Removed on next deploy, with ${mounts} mount${mounts === 1 ? "" : "s"}.` : "Removed on next deploy."}
        action={<>
        {removing ? (
          <>
            {/* The staged removal goes; the Volume stays as deployed. */}
            <Button variant="outline" onClick={() => actions.discard(`volumes.${volume.name}`)}>Keep volume</Button>
            {volume.deployed ? (
              <Button variant="destructive" disabled={actions.admitting} onClick={() => actions.deploy(null)}>
                <Trash2Icon data-icon="inline-start" />
                Delete data
              </Button>
            ) : null}
          </>
        ) : (
          <Button variant="destructive" onClick={remove}>
            <Trash2Icon data-icon="inline-start" />
            Delete volume
          </Button>
        )}
        </>} />
      {actions.dialog}
    </>
  );
}

/**
 * Whether more than one container may write this volume. Off by default, and the Store then refuses a second writer.
 * Applies at once, not staged. Turning it off with several writers is refused; the reason shows here.
 */
function StoreSharedWrites({ state }: { state: StoreVolume }) {
  const writer = useStoreWriter(state.organizationSlug);
  const [refusal, setRefusal] = useState<string | null>(null);
  return (
    <Field orientation="horizontal" data-invalid={refusal ? true : undefined}>
      <FieldContent>
        <FieldLabel htmlFor="volume-shared-writes">Allow shared writes</FieldLabel>
        <FieldDescription>
          Lets more than one container write here.{" "}
          <span className="inline-flex align-middle"><InfoHint>{SHARED_VOLUME_WHY}</InfoHint></span>
        </FieldDescription>
        {refusal ? <FieldError>{refusal}</FieldError> : null}
      </FieldContent>
      <Switch id="volume-shared-writes" checked={state.volume.shared_writes}
        onCheckedChange={(sharedWrites) => {
          setRefusal(null);
          writer.commit({ command: "set_volume_shared_writes", environment: state.environment, volume: state.volume.name, shared_writes: sharedWrites })
            .isPersisted.promise.catch((error: Error) => { if (error instanceof StoreRefused) setRefusal(error.message); });
        }} />
    </Field>
  );
}
