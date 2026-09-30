import { useState } from "react";
import { Schema } from "effect";
import { useLoaderData, useNavigate } from "@tanstack/react-router";
import { HardDriveIcon, PencilIcon, PlusIcon, Trash2Icon } from "lucide-react";
import type { DiffView, EnvironmentRef, Mount, ServiceListing, VolumeListing } from "@ployz/sdk";
import { Button } from "#/components/ui/button";
import { Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from "#/components/ui/empty";
import { Field, FieldError, FieldLabel } from "#/components/ui/field";
import { Input } from "#/components/ui/input";
import { Item, ItemActions, ItemContent, ItemDescription, ItemTitle } from "#/components/ui/item";
import { Select, SelectContent, SelectGroup, SelectItem, SelectTrigger, SelectValue } from "#/components/ui/select";
import { Separator } from "#/components/ui/separator";
import { diffQuery, requireView, servicesQuery, useStoreViews, volumesQuery } from "#/modules/config-store/store-view.queries";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { detachedMounts, gigabytes, mountChange, mountPathError, volumeStorage, volumeStorageText } from "#/modules/config-store/store-volumes";
import { VolumeStorageFields } from "#/modules/config-store/VolumeStorageFields";
import { CanvasInspectorHeader } from "../../../-components/CanvasInspectorHeader";
import { CanvasInspectorNotFound } from "../../../-components/CanvasInspectorRouteStates";
import { CanvasInspectorNameEditor } from "../../../-components/CanvasInspectorNameEditor";
import { dnsLabelError, settingText } from "#/modules/config-store/store-services";
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
  const views = useStoreViews(organizationSlug, [volumesQuery(store), servicesQuery(store), diffQuery(store)] as const);
  const volumes = requireView(views[0]).volumes;
  const services = requireView(views[1]).services;
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
            isChanged={renamed !== undefined} baselineValue={renamed ? settingText(renamed.before) : undefined}
            onRename={(name) => writer.commit({ command: "rename_volume", environment: store, volume: volume.name, name })} />
        )}
        <p className="truncate text-sm text-muted-foreground">Volume</p>
      </CanvasInspectorHeader>
      <div className="min-h-0 flex-1 overflow-y-auto px-6 py-6">
        <StoreVolumeStorage key={`${volume.id}:${JSON.stringify(volume.storage)}`} state={state} removing={removing} />
        <div className="py-8"><Separator /></div>
        <section aria-labelledby="volume-mounts-heading">
          <h2 id="volume-mounts-heading" className="text-lg font-semibold">Mounts</h2>
          <div className="mt-4">
            {removing ? (
              <Empty>
                <EmptyHeader>
                  <EmptyMedia variant="icon"><HardDriveIcon /></EmptyMedia>
                  <EmptyTitle>Volume staged for deletion</EmptyTitle>
                  <EmptyDescription>Keep the volume, below, to manage its mounts again.</EmptyDescription>
                </EmptyHeader>
              </Empty>
            ) : <StoreVolumeMounts state={state} services={services} volumes={volumes} diff={diff} />}
          </div>
        </section>
        <div className="py-8"><Separator /></div>
        <StoreVolumeDanger state={state} params={params} version={diff.version} />
      </div>
    </div>
  );
}

function StoreVolumeStorage({ state, removing }: { state: StoreVolume; removing: boolean }) {
  const writer = useStoreWriter(state.organizationSlug);
  const { storage, storage_locked } = state.volume;
  const [managed, setManaged] = useState(storage.kind === "provisioned");
  const [sizeGB, setSizeGB] = useState(storage.kind === "provisioned" ? gigabytes(storage.maximumBytes) : "5");
  const [error, setError] = useState<string | null>(null);
  const next = volumeStorage(managed, sizeGB);
  const changed = next !== null && (next.kind !== storage.kind || (next.kind === "provisioned"
    && storage.kind === "provisioned" && next.maximumBytes !== storage.maximumBytes));

  function save(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!next) return setError("Enter a valid storage limit of at least 0.001 GB.");
    if (!changed) return;
    writer.commit({ command: "set_volume_storage", environment: state.environment,
      volume: state.volume.name, storage: next });
  }

  return <section aria-labelledby="volume-storage-heading" className="flex flex-col gap-4">
    <h2 id="volume-storage-heading" className="text-lg font-semibold">Storage</h2>
    <p className="text-sm text-muted-foreground">Files stored here survive deployments and restarts. Data stays on the server that hosts this volume.</p>
    {storage_locked || removing ? <>
      <p className="text-sm">{managed ? "Managed · " : ""}{volumeStorageText(storage)}</p>
      <p className="text-sm text-muted-foreground">Storage settings are fixed once deployment is requested.</p>
    </> : <form onSubmit={save} className="flex flex-col items-start gap-4">
      <VolumeStorageFields managed={managed} sizeGB={sizeGB} onManagedChange={setManaged}
        onSizeChange={(value) => { setSizeGB(value); setError(null); }} error={error} />
      <Button type="submit" disabled={!changed}>Save storage</Button>
    </form>}
  </section>;
}

function StoreVolumeMounts({ state, services, volumes, diff }: {
  state: StoreVolume; services: readonly ServiceListing[]; volumes: readonly VolumeListing[]; diff: DiffView;
}) {
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
      {state.volume.mounts.length === 0 && detached.length === 0 ? (
        <Empty>
          <EmptyHeader>
            <EmptyMedia variant="icon"><HardDriveIcon /></EmptyMedia>
            <EmptyTitle>No mounts yet</EmptyTitle>
            <EmptyDescription>Mount this volume on a service to give it persistent storage.</EmptyDescription>
          </EmptyHeader>
        </Empty>
      ) : (
        <div className="flex flex-col gap-2">
          {state.volume.mounts.map((mount) => (
            <StoreMountItem key={mount.service} mount={mount} onPath={(path) => edit(mount.service, path)} onDetach={() => edit(mount.service, null)} />
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
        <p className="text-sm text-muted-foreground">Every service in this environment already mounts this volume.</p>
      ) : (
        <Field data-invalid={adding.error ? true : undefined}>
          <FieldLabel>Mount on a service</FieldLabel>
          <div className="flex gap-2">
            <Select value={adding.service} onValueChange={(service) => setAdding({ ...adding, service: service ?? "", error: null })}>
              <SelectTrigger className="flex-1" aria-label="Service"><SelectValue placeholder="Select a service" /></SelectTrigger>
              <SelectContent>
                <SelectGroup>
                  {available.map((service) => <SelectItem key={service.id} value={service.name}>{service.name}</SelectItem>)}
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
function StoreMountItem({ mount, onPath, onDetach }: { mount: Mount; onPath: (path: string) => void; onDetach: () => void }) {
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
        <ItemTitle>{mount.service}</ItemTitle>
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
    <section aria-labelledby="volume-danger-heading">
      <h2 id="volume-danger-heading" className="text-lg font-semibold text-destructive">Danger</h2>
      <div className="mt-4 flex flex-col items-start justify-between gap-4 rounded-xl border border-destructive-border bg-destructive-soft p-4 sm:flex-row sm:items-center">
        <div className="min-w-0">
          <div className="text-sm font-semibold text-destructive">
            {removing && volume.deployed ? "Delete its data" : removing ? "Deleted on your next deploy" : "Delete this volume"}
          </div>
          <p className="mt-1 text-sm text-destructive/85">
            {removing && volume.deployed
              ? "Its files are still on your servers. Deploying deletes them, with this environment's other changes, once you confirm."
              : removing
              ? "Nothing of it is on your servers yet. Keep the volume to undo this."
              : volume.deployed
              ? "Its data on your servers goes with it. Deploy asks you to confirm first."
              : mounts > 0 ? `Deleted on your next deploy, with its ${mounts} mount${mounts === 1 ? "" : "s"}.` : "Deleted on your next deploy."}
          </p>
        </div>
        {removing ? (
          <div className="flex shrink-0 gap-2">
            {/* The staged removal goes; the Volume stays as deployed. */}
            <Button variant="outline" onClick={() => actions.discard(`volumes.${volume.name}`)}>Keep volume</Button>
            {volume.deployed ? (
              <Button variant="destructive" disabled={actions.admitting} onClick={() => actions.deploy(null)}>
                <Trash2Icon data-icon="inline-start" />
                Delete data
              </Button>
            ) : null}
          </div>
        ) : (
          <Button variant="destructive" className="shrink-0" onClick={remove}>
            <Trash2Icon data-icon="inline-start" />
            Delete volume
          </Button>
        )}
      </div>
      {actions.dialog}
    </section>
  );
}
