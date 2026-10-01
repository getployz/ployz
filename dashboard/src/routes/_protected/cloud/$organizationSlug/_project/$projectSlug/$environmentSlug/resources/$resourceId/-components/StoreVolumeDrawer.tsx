import { useState } from "react";
import { Schema } from "effect";
import { useLoaderData, useNavigate } from "@tanstack/react-router";
import { PencilIcon, PlusIcon, Trash2Icon } from "lucide-react";
import type { DiffView, EnvironmentRef, Mount, ServiceListing, VolumeListing } from "@ployz/sdk";
import { Button } from "#/components/ui/button";
import { Empty, EmptyDescription } from "#/components/ui/empty";
import { Field, FieldContent, FieldDescription, FieldError, FieldLabel, FieldTitle } from "#/components/ui/field";
import { Input } from "#/components/ui/input";
import { Item, ItemActions, ItemContent, ItemDescription, ItemTitle } from "#/components/ui/item";
import { Select, SelectContent, SelectGroup, SelectItem, SelectTrigger, SelectValue } from "#/components/ui/select";
import { diffQuery, environmentSettingsQuery, requireView, servicesQuery, useStoreViews, volumesQuery } from "#/modules/config-store/store-view.queries";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { DEFAULT_VOLUME_GB, detachedMounts, gigabytes, mountChange, mountPathError, volumeStorage, volumeStorageText } from "#/modules/config-store/store-volumes";
import { VolumeAdvanced, VolumeStorageFields } from "#/modules/config-store/VolumeStorageFields";
import { CanvasInspectorHeader } from "../../../-components/CanvasInspectorHeader";
import { mountRefusal, replicaCount, volumeWriters, writersText } from "#/modules/config-store/volume-sharing";
import { StoreRefused } from "#/modules/config-store/store.contract";
import { InfoHint } from "#/components/info-hint";
import { Switch } from "#/components/ui/switch";
import { serviceSettingRows } from "#/modules/config-store/store-services";
import { Badge } from "#/components/ui/badge";
import { RowWarning, SettingsSection, SHARED_VOLUME_WHY } from "#/routes/_protected/cloud/$organizationSlug/-components/SettingsSection";
import { DangerRow } from "#/routes/_protected/cloud/$organizationSlug/-components/danger-row";
import { ServiceSettingInput } from "../../../services/$serviceId/-components/ServiceSettingInput";
import { CanvasInspectorNotFound } from "../../../-components/CanvasInspectorRouteStates";
import { CanvasInspectorNameEditor } from "../../../-components/CanvasInspectorNameEditor";
import { changedProps, dnsLabelError } from "#/modules/config-store/store-services";
import { useStoreChangeActions } from "../../../-components/canvas/useStoreChangeActions";
import { DEPLOYMENT_PAGE_ROUTE_TO } from "../../../-components/deployment-page";
import { ENVIRONMENT_INDEX_ROUTE_TO, ENVIRONMENT_ROUTE_FROM } from "../../../-components/environment-route-paths";

type StoreVolume = { organizationSlug: string; environment: EnvironmentRef; volume: VolumeListing };

/**
 * A Volume in the Config Store: where Services mount it, and its removal. Mount edits and removal are staged like any
 * other change; the data goes only with a Deploy that removes a deployed Volume, which asks first.
 */
type VolumeResourceRouteParams = { organizationSlug: string; projectSlug: string; environmentSlug: string; resourceId: string };

export function StoreVolumeDrawer({ params }: { params: VolumeResourceRouteParams }) {
  const { store } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const { organizationSlug } = params;
  const views = useStoreViews(organizationSlug, [volumesQuery(store), servicesQuery(store), diffQuery(store), environmentSettingsQuery(store)] as const);
  const volumes = requireView(views[0]).volumes;
  const services = requireView(views[1]).services;
  const settings = requireView(views[3]);
  // Staged counts count: a shared volume warns before the Deploy that would share it.
  const replicasOf = (service: string) => replicaCount(serviceSettingRows(settings, service).get("replicas"));
  const diff = requireView(views[2]);
  const writer = useStoreWriter(organizationSlug);
  const volume = volumes.find((candidate) => candidate.id === params.resourceId);
  // A stale link, or removed while open (a new Volume's removal, or from the CLI).
  if (!volume) return <CanvasInspectorNotFound noun="Volume" />;
  const state: StoreVolume = { organizationSlug, environment: store, volume };
  const removing = volume.change === "delete";
  const renamed = diff.changes.find((change) => change.type === "volume" && change.id === volume.id)
    ?.settings.find((row) => row.path === "name" || row.path.endsWith(".name"));
  // A DNS label no other Volume here has.
  const nameSchema = Schema.String.check(Schema.makeFilter<string>((name) => dnsLabelError(name)
    ?? (volumes.some((other) => other.id !== volume.id && other.name === name) ? `A volume here is already named ${name}.` : undefined)));

  return (
    <div className="flex h-full min-h-0 flex-col">
      <CanvasInspectorHeader params={params}>
        {removing ? <p className="truncate font-semibold">{volume.name}</p> : (
          <CanvasInspectorNameEditor value={volume.name} schema={nameSchema} editTitle="Edit volume name"
            editDescription="Rename this volume. Its data and mounts stay." placeholder="Volume name"
            {...changedProps(renamed)}
            onRename={(name) => writer.commit({ command: "rename_volume", environment: store, volume: volume.name, name })} />
        )}
        {/* What it is and who uses it, before any setting. */}
        <p className="truncate text-sm text-muted-foreground">
          {[volumeStorageText(volume.storage), volume.deployed ? null : "not deployed",
            volume.mounts.length ? `mounted by ${volume.mounts.map((mount) => `${mount.service} at ${mount.path}`).join(", ")}` : "not mounted"]
            .filter(Boolean).join(" · ")}
        </p>
      </CanvasInspectorHeader>
      <div className="min-h-0 flex-1 overflow-y-auto px-4 pt-4 pb-8">
        <div className="mx-auto flex w-full max-w-2xl flex-col gap-6">
          <SettingsSection id="mounts" title="Mounts">
            {removing ? (
              <Empty variant="placeholder">
                <EmptyDescription>Removed on next deploy.</EmptyDescription>
              </Empty>
            ) : <StoreVolumeMounts state={state} services={services} volumes={volumes} diff={diff} replicasOf={replicasOf} />}
          </SettingsSection>
          <SettingsSection id="storage" title="Storage">
            <StoreVolumeStorage key={`${volume.id}:${JSON.stringify(volume.storage)}`} state={state} removing={removing} />
          </SettingsSection>
          <SettingsSection id="danger" title="Danger">
            <StoreVolumeDanger state={state} params={params} version={diff.version} />
          </SettingsSection>
        </div>
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

function StoreVolumeMounts({ state, services, volumes, diff, replicasOf }: {
  state: StoreVolume; services: readonly ServiceListing[]; volumes: readonly VolumeListing[]; diff: DiffView;
  replicasOf: (service: string) => number;
}) {
  const { shared, writers, total } = volumeWriters(state.volume, replicasOf);
  // One Service's replicas are the only writers: fewer replicas fixes it here. Otherwise a mount goes, below.
  const only = writers.length === 1 ? writers[0] : null;
  const writer = useStoreWriter(state.organizationSlug);
  const [adding, setAdding] = useState<{ service: string; path: string; error: string | null }>({ service: "", path: "/data", error: null });
  const mounted = new Set(state.volume.mounts.map((mount) => mount.service));
  // A Service being removed can't gain a mount.
  const available = services.filter((service) => !mounted.has(service.name) && service.change !== "delete");
  const detached = detachedMounts(diff, state.volume.name);
  const edit = (service: string, path: string | null) =>
    writer.edit({ environment: state.environment, changes: [mountChange(service, state.volume.name, path)] });

  function attach() {
    // Another Volume at the same path in that Service would hide one of them.
    const taken = volumes.some((other) => other.id !== state.volume.id
      && other.mounts.some((mount) => mount.service === adding.service && mount.path === adding.path));
    const error = adding.service === "" ? "Select a service."
      : mountPathError(adding.path) ?? (taken ? `Another volume is already mounted at ${adding.path}.` : null);
    if (error) return setAdding({ ...adding, error });
    edit(adding.service, adding.path);
    setAdding({ service: "", path: "/data", error: null });
  }

  return (
    <div className="flex flex-col gap-6">
      {shared ? (
        <RowWarning why={SHARED_VOLUME_WHY} action={only ? (
          <Button type="button" variant="outline" size="xs"
            onClick={() => writer.edit({ environment: state.environment, changes: [{ op: "set", path: `${only.service}.replicas`, value: 1 }] })}>
            Use 1 replica
          </Button>
        ) : null}>
          {total} containers write here: {writersText(writers)}. Can corrupt data.
        </RowWarning>
      ) : null}
      {state.volume.mounts.length === 0 && detached.length === 0 ? (
        <Empty variant="placeholder">
          <EmptyDescription>Not mounted.</EmptyDescription>
        </Empty>
      ) : (
        <div className="flex flex-col gap-2">
          {state.volume.mounts.map((mount) => (
            <StoreMountItem key={mount.service} mount={mount} replicas={replicasOf(mount.service)}
              onPath={(path) => edit(mount.service, path)} onDetach={() => edit(mount.service, null)} />
          ))}
          {detached.map((mount) => (
            <Item key={mount.service} variant="muted">
              <ItemContent>
                <ItemTitle>{mount.service}</ItemTitle>
                <ItemDescription>Unmounts from {mount.path} on your next deploy. The data stays in this volume.</ItemDescription>
              </ItemContent>
            </Item>
          ))}
        </div>
      )}
      {available.length === 0 ? (
        <p className="text-sm text-muted-foreground">{services.some((service) => service.change !== "delete")
          ? "Every service mounts it."
          : "Add a service to mount this volume."}</p>
      ) : (
        <Field data-invalid={adding.error ? true : undefined}>
          <FieldLabel>Mount on a service</FieldLabel>
          <div className="flex flex-col gap-2 sm:flex-row">
            <Select value={adding.service} onValueChange={(service) => setAdding({ ...adding, service: service ?? "", error: null })}>
              <SelectTrigger className="w-full sm:w-auto sm:flex-1" aria-label="Service"><SelectValue placeholder="Select a service" /></SelectTrigger>
              <SelectContent>
                <SelectGroup>
                  {available.map((service) => {
                    // A second writer the volume doesn't allow: greyed, with why, instead of refused after the fact.
                    const refusal = mountRefusal(state.volume, service.name, replicasOf(service.name));
                    return (
                      <SelectItem key={service.id} value={service.name} disabled={refusal !== null} label={service.name}>
                        <span className="flex w-full items-center justify-between gap-3">
                          <span>{service.name}</span>
                          {refusal ? <span className="text-muted-foreground">{refusal}</span> : null}
                        </span>
                      </SelectItem>
                    );
                  })}
                </SelectGroup>
              </SelectContent>
            </Select>
            <Input className="flex-1" aria-label="Mount path" value={adding.path} placeholder="/data"
              aria-invalid={adding.error ? true : undefined}
              onChange={(event) => setAdding({ ...adding, path: event.target.value, error: null })} />
            <Button onClick={attach}><PlusIcon data-icon="inline-start" />Mount</Button>
          </div>
          {adding.error ? <FieldError>{adding.error}</FieldError> : null}
        </Field>
      )}
    </div>
  );
}

/** One Service's mount: its path, edited in place, and detaching, which leaves the Volume and its data. */
function StoreMountItem({ mount, replicas, onPath, onDetach }: { mount: Mount; replicas: number; onPath: (path: string) => void; onDetach: () => void }) {
  const [editing, setEditing] = useState<{ path: string; error: string | null } | null>(null);

  function save() {
    if (!editing) return;
    const error = mountPathError(editing.path);
    if (error) return setEditing({ ...editing, error });
    if (editing.path !== mount.path) onPath(editing.path);
    setEditing(null);
  }

  return (
    <Item variant="outline">
      <ItemContent>
        <ItemTitle>{mount.service}{replicas > 1 ? <Badge variant="warning">{replicas} replicas</Badge> : null}</ItemTitle>
        {editing ? (
          <Field data-invalid={editing.error ? true : undefined}>
            <Input value={editing.path} aria-label="Mount path" aria-invalid={editing.error ? true : undefined} autoFocus
              onChange={(event) => setEditing({ path: event.target.value, error: null })}
              onKeyDown={(event) => { if (event.key === "Enter") save(); }} />
            {editing.error ? <FieldError>{editing.error}</FieldError> : null}
          </Field>
        ) : <ItemDescription className="truncate">{mount.path}</ItemDescription>}
      </ItemContent>
      <ItemActions>
        {editing ? (
          <>
            <Button variant="outline" size="sm" onClick={() => setEditing(null)}>Cancel</Button>
            <Button size="sm" onClick={save}>Save</Button>
          </>
        ) : (
          <>
            <Button variant="ghost" size="icon-sm" aria-label="Edit mount path" onClick={() => setEditing({ path: mount.path, error: null })}>
              <PencilIcon />
            </Button>
            {/* Detaching keeps the Volume and its data; only deleting the Volume can lose it. */}
            <Button variant="ghost" size="icon-sm" aria-label="Remove mount" title="Remove mount (keeps the data)" onClick={onDetach}>
              <Trash2Icon />
            </Button>
          </>
        )}
      </ItemActions>
    </Item>
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
