import { StoreRefused } from "#/modules/config-store/store.contract";
import { Suspense, useState, type ComponentProps, type ReactNode } from "react";
import { Link, useLoaderData, useNavigate, useSearch } from "@tanstack/react-router";
import { Schema } from "effect";
import { EllipsisIcon, PackageIcon, PencilIcon, PlusIcon, Trash2Icon, XIcon } from "lucide-react";
import type { Change, EnvironmentRef, JsonValue, ServiceListing, ServiceSettingChange, SettingRow, VolumeListing } from "@ployz/sdk";
import { effectiveVolumes } from "#/modules/config-store/store-volumes";
import { replicaCap, replicaCount } from "#/modules/config-store/volume-sharing";
import { InfoHint } from "#/components/info-hint";
import { GitRepoSelectorDialog, ImageSelectorDialog } from "#/components/service-source-selector";
import { GitHubMarkIcon } from "#/components/icons/github-mark";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Input } from "#/components/ui/input";
import { FieldError } from "#/components/ui/field";
import { Switch } from "#/components/ui/switch";
import { DropdownMenu, DropdownMenuContent, DropdownMenuGroup, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "#/components/ui/dropdown-menu";
import { sourceText } from "#/modules/config-store/store-branches";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "#/components/ui/tabs";
import { Select, SelectContent, SelectGroup, SelectItem, SelectTrigger, SelectValue } from "#/components/ui/select";
import { scalarSetting, serviceSetting, settingChange, settingError, type ServiceSettingName, type SettingSchema } from "#/modules/config-store/catalog";
import { templateLabel } from "#/modules/config-store/database-presets";
import { changedProps, deployedVcpuLimit, dnsLabelError, serviceChanges, serviceSettingRows, settingText } from "#/modules/config-store/store-services";
import { diffQuery, environmentSettingsQuery, namespaceQuery, requireView, servicesQuery, useStoreViews, volumesQuery } from "#/modules/config-store/store-view.queries";
import type { Persistable } from "#/collections/query-collection";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { useCommitHold } from "#/components/stageable/use-commit-hold";
import { useStorePendingName, useStorePendingNodes } from "#/modules/config-store/store-pending";
import { SettingsAddButton, SettingsDanger, SettingsFoot, SettingsRow, SettingsSection } from "#/components/settings/settings-section";
import { SettingsEmpty, SettingsList, SettingsListItem } from "#/components/settings/settings-list";
import { SettingsDisclosure } from "#/components/settings/settings-disclosure";
import { SettingsField, SettingsGroup } from "#/components/settings/settings-group";
import { RowWarning, SHARED_VOLUME_WHY } from "#/components/settings/row-warning";
import { CanvasInspectorHeader } from "../../../-components/CanvasInspectorHeader";
import { CanvasInspectorNotFound, CanvasInspectorPending } from "../../../-components/CanvasInspectorRouteStates";
import { CanvasInspectorNameEditor } from "../../../-components/CanvasInspectorNameEditor";
import { ENVIRONMENT_RESOURCE_ROUTE_TO, ENVIRONMENT_ROUTE_FROM } from "../../../-components/environment-route-paths";
import { RegistryCredentialsField } from "./ServiceRegistryCredentialsSection";
import { ServiceSettingInput } from "./ServiceSettingInput";
import { ServiceCommandField } from "./ServiceCommandField";
import { HealthcheckSettings } from "./HealthcheckField";
import { StoreBranchField, StoreDockerfileField, StorePreferredBuilderField, useRepositoryRef } from "./StoreGitFields";
import { SERVICE_SETTINGS_SECTIONS, type ServiceSettingsSectionId } from "./service-settings-sections";
import { useRemoveStoreService } from "./useDeleteService";
import { StoreServiceVariablesTab } from "./ServiceVariablesTab";
import { StoreNetworkingSection } from "./StoreNetworkingSection";
import { useServiceRuntime } from "./useServiceRuntime";
import { ServiceActAlert, ServiceActions, useServiceAct } from "./ServiceActions";
import { ContainerLogs } from "#/components/container-logs";
import { Skeleton } from "#/components/ui/skeleton";
import { ItemGroup } from "#/components/ui/item";
import { StoreDeploymentRows } from "../../../-components/DeploymentsList";
import { originFrom } from "../../../-components/deployments/run-search";
import { SERVICE_PAGES, servicePageSchema } from "./service-pages";
import { DEFAULT_RANGE } from "#/modules/uptime/uptime-range";
import { usePrefetchServiceUptime, useUptimeEnabled } from "#/modules/uptime/uptime.queries";
import { MonitoringTab } from "./monitoring/MonitoringTab";
import { useGithubWebOrigin } from "#/modules/github/github.queries";

/** One Service in the Config Store, as the drawer shows and edits it. */
export type StoreService = {
  organizationSlug: string;
  environment: EnvironmentRef;
  service: ServiceListing;
  /** Its Settings, defaults included, with this tab's pending edits over them. */
  rows: Map<string, SettingRow>;
  /** What the next Deploy changes, by Setting: the pink trail. */
  changes: Map<string, ServiceSettingChange>;
  /** Where its image comes from, with pending edits: connecting or disconnecting a source shows at once. */
  source: ServiceListing["source"];
  /** Setting `name`'s Store path: `SERVICE.name`. */
  path: (name: string) => string;
  /** Edits it at once, saved in the background. */
  edit: (change: Change) => Persistable;
  /** Sets Setting `name`, or unsets it (null). */
  set: (name: string, value: JsonValue | null) => Persistable;
};

/** A Service's source as its Settings say, pending edits included; an upload only while it has none of its own. */
function sourceOf(service: ServiceListing, rows: Map<string, SettingRow>): ServiceListing["source"] {
  if (settingText(rows.get("repository")?.value)) return "git";
  if (settingText(rows.get("image")?.value)) return "image";
  return service.source === "uploaded" ? "uploaded" : "empty";
}

const OPTION_LABELS = new Map([
  ["unless-stopped", "Unless stopped"],
  ["always", "Always"],
  ["on-failure", "On failure"],
  ["no", "Never"],
  ["railpack", "Railpack"],
  ["dockerfile", "Dockerfile"],
]);

const SERVICE_ROUTE_FROM = "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/services/$serviceId";
const SERVICE_ROUTE_TO = "/cloud/$organizationSlug/$projectSlug/$environmentSlug/services/$serviceId";

/** Whether another Service here is named or reached as `name`: Service names and Private DNS names share one space. */
const taken = (service: ServiceListing, services: readonly ServiceListing[], name: string) =>
  services.some((other) => other.id !== service.id && (other.name === name || other.private_dns === name));

function nameSchema(service: ServiceListing, services: readonly ServiceListing[]) {
  return Schema.String.check(Schema.makeFilter<string>((name) =>
    dnsLabelError(name) ?? (taken(service, services, name) ? `A service here is already named ${name}.` : undefined)));
}

/**
 * A Service's drawer over the Config Store: its name, source and scalar Settings, labelled by the catalog. Edits save
 * through the Environment's queue; a rename or removal is staged like any other change.
 */

/**
 * A Service's drawer over the Config Store: its name, source and scalar Settings, labelled by the catalog. Edits save
 * through the Environment's queue; a rename or removal is staged like any other change.
 */
export function StoreServiceDrawer({ params }: { params: { organizationSlug: string; projectSlug: string; environmentSlug: string; serviceId: string } }) {
  const { store, environmentId } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const { organizationSlug } = params;
  const views = useStoreViews(organizationSlug,
    [servicesQuery(store), environmentSettingsQuery(store), diffQuery(store), volumesQuery(store), namespaceQuery(store)] as const);
  const services = requireView(views[0]).services;
  const settings = requireView(views[1]);
  const diff = requireView(views[2]);
  const volumes = effectiveVolumes(requireView(views[3]).volumes, services, settings);
  const namespace = views[4].ok ? views[4].value.namespace : null;
  const writer = useStoreWriter(organizationSlug);
  const { tab, from = DEFAULT_RANGE.from, to = DEFAULT_RANGE.to } = useSearch({ from: SERVICE_ROUTE_FROM });
  const range = { from, to };
  const prefetchUptime = usePrefetchServiceUptime({ organizationSlug, environmentId }, params.serviceId);
  const uptimeEnabled = useUptimeEnabled(organizationSlug);
  const navigate = useNavigate({ from: SERVICE_ROUTE_TO });
  const service = services.find((candidate) => candidate.id === params.serviceId);
  const act = useServiceAct({ organizationSlug, environmentId, service: service?.name ?? "" });
  const shownName = useStorePendingName(organizationSlug, store, "service", service?.name ?? "");
  const arriving = useStorePendingNodes(organizationSlug, store).arriving.some((ghost) => ghost.id === params.serviceId);
  // A stale link, or removed while open (from the CLI; the drawer's own Delete closes it).
  if (!service) return arriving ? <CanvasInspectorPending /> : <CanvasInspectorNotFound noun="Service" />;

  const rows = serviceSettingRows(settings, service.name);
  const path = (name: string) => `${service.name}.${name}`;
  const edit = (change: Change) => writer.edit({ environment: store, changes: [change] });
  const state: StoreService = {
    organizationSlug,
    environment: store,
    service,
    rows,
    changes: serviceChanges(diff, service.id),
    source: sourceOf(service, rows),
    path,
    edit,
    set: (name, value) => edit(value === null ? { op: "unset", path: path(name) } : { op: "set", path: path(name), value }),
  };
  const database = service.template != null;
  const replicasOf = (name: string) => replicaCount(serviceSettingRows(settings, name).get("replicas"));
  // A volume without shared writes holds this service to one replica. More than one already (from before the rule)
  // isn't capped: its volume's writers warning says so, with the fix.
  const cap = replicaCap(service.name, volumes);
  const capped = cap && replicasOf(service.name) <= 1 ? cap : null;
  const hasVolume = volumes.some((volume) => volume.mounts.some((mount) => mount.service === service.name));
  const managedVolume = volumes.find((volume) => volume.storage.kind === "provisioned" && volume.mounts.some((mount) => mount.service === service.name)) ?? null;
  const vcpuLimit = deployedVcpuLimit(rows, state.changes);
  const rename = state.changes.get("name");
  const restartPolicy = state.rows.get("restartPolicy");
  const buildMethod = settingText(state.rows.get("buildMethod")?.value ?? state.rows.get("buildMethod")?.default);
  const field = (name: ServiceSettingName) => <StoreSettingField key={name} state={state} name={name} />;
  const healthcheck = {
    value: state.rows.get("healthcheck")?.value, change: state.changes.get("healthcheck"),
    set: (value: JsonValue | null) => state.set("healthcheck", value),
  };
  const bodies = {
    source: <StoreSourceSection state={state} />,
    // Domain statuses need a look at the Cluster, so the rest of the drawer doesn't wait for them.
    networking: (
      <Suspense fallback={null}>
        <StoreNetworkingSection state={state} privateFirst={database}
          validatePrivateDns={(raw) => raw === "" ? null : settingError(serviceSetting("privateDns"), raw)
            ?? (taken(service, services, raw) ? `A service here is already reached as ${raw}.` : null)} />
      </Suspense>
    ),
    storage: database && !hasVolume ? <SettingsRow label="Volume" commit="none"><RowWarning>No volume. Data is lost on redeploy.</RowWarning></SettingsRow> : null,
    scale: <>
      {capped ? <StoreReplicasCapped params={params} volume={capped} /> : field("replicas")}
      <StoreResourceLimits state={state} />
    </>,
    build: state.source === "git" ? <>
      {field("buildMethod")}
      {buildMethod === "dockerfile" ? <StoreDockerfile state={state} /> : field("buildCommand")}
      <StorePreferredBuilderField organizationSlug={organizationSlug} value={policyText(state.rows.get("preferredBuilder")?.value)}
        onSet={(builder) => state.set("preferredBuilder", builder)} />
    </> : null,
    deploy: <>
      {field("startCommand")}
      <StoreOptionalField state={state} name="preDeployCommand" addLabel="Pre-deploy command" />
      <HealthcheckSettings {...healthcheck} />
      {field("restartPolicy")}
      {(restartPolicy?.value ?? restartPolicy?.default) === "on-failure" ? field("maxRetries") : null}
    </>,
    danger: <StoreDangerSection state={state} />,
  } satisfies Record<ServiceSettingsSectionId, ReactNode>;

  return (
    <div className="flex h-full min-h-0 flex-col">
      <CanvasInspectorHeader params={params}
        actions={<ServiceActions state={state} namespace={namespace} act={act} />}>
        <div className="min-w-0"><CanvasInspectorNameEditor
          value={shownName}
          schema={nameSchema(service, services)}
          editTitle="Edit service name"
          editDescription="Rename this service. Other services reach it at its Private DNS name, which stays."
          placeholder="Service name"
          {...changedProps(rename)}
          onRename={(name) => writer.commit({ command: "rename_service", environment: store, service: service.name, name })}
        /></div>
      </CanvasInspectorHeader>
      <div className="flex min-h-0 flex-1 flex-col px-4 pb-4">
        <ServiceActAlert act={act} />
        <Tabs value={tab ?? "settings"}
          onValueChange={(value) => {
            if (Schema.is(servicePageSchema)(value)) void navigate({ search: (prev) => ({ ...prev, tab: value }), replace: true });
          }}
          className="flex min-h-0 flex-1 flex-col overflow-visible sm:overflow-clip">
          <TabsList variant="line" className="-mx-4 max-w-none shrink-0 overflow-x-auto sm:mx-0 sm:max-w-full">
            {SERVICE_PAGES.map((page) => (
              <TabsTrigger key={page.id} value={page.id}
                onPointerEnter={page.id === "monitoring" ? () => prefetchUptime(range) : undefined}>{page.label}</TabsTrigger>
            ))}
          </TabsList>
          <TabsContent value="deployments" className="mt-4 min-h-0 flex-1 overflow-y-auto">
            <nav aria-label="Deployments" className="mx-auto w-full max-w-2xl">
              <ItemGroup className="gap-1">
                <Suspense fallback={<Skeleton className="h-16 w-full" />}>
                  <StoreDeploymentRows service={service} origin={originFrom(params, { kind: "service", id: service.id })} />
                </Suspense>
              </ItemGroup>
            </nav>
          </TabsContent>
          <TabsContent value="logs" className="mt-4 flex min-h-0 flex-1 flex-col">
            <ContainerLogs selection={{ organizationSlug, projectSlug: params.projectSlug, environmentSlug: params.environmentSlug, serviceId: service.id }} />
          </TabsContent>
          <TabsContent value="monitoring" className="mt-3 min-h-0 flex-1 overflow-clip">
            <div className="h-full overflow-y-auto pr-1 pb-8">
              <div className="mx-auto w-full max-w-2xl">
                <ServiceMonitoring state={state} namespace={namespace} organizationSlug={organizationSlug} checksUptime={uptimeEnabled} service={service} range={range}
                  vcpuLimit={vcpuLimit} volume={managedVolume}
                  onRange={(next) => {
                    const fallback = next.from === DEFAULT_RANGE.from && next.to === DEFAULT_RANGE.to;
                    void navigate({ search: (prev) => ({ ...prev, from: fallback ? undefined : next.from, to: fallback ? undefined : next.to }), replace: true });
                  }}
                  healthcheck={healthcheck} />
              </div>
            </div>
          </TabsContent>
          <TabsContent value="settings" className="mt-3 min-h-0 flex-1 overflow-clip">
            <div className="h-full overflow-y-auto pr-1 pb-8">
              <div className="mx-auto flex w-full max-w-2xl flex-col gap-6">
                {SERVICE_SETTINGS_SECTIONS.flatMap((section) => {
                  const body = bodies[section.id];
                  return body ? [(
                    <SettingsSection key={section.id} id={section.id} title={section.label}>
                      {body}
                    </SettingsSection>
                  )] : [];
                })}
              </div>
            </div>
          </TabsContent>
          <StoreServiceVariablesTab organizationSlug={organizationSlug} environment={store} service={service} services={services} settings={settings} changes={state.changes} />
        </Tabs>
      </div>
    </div>
  );
}

/** Optional Settings a blank input leaves unset: commands, and the root directory (whose `/` is the default). */
const COMMANDS = new Map<ServiceSettingName, { placeholder: string; unset?: string }>([
  // Placeholders say what happens while unset, never a guess at this Service's stack.
  ["startCommand", { placeholder: "Image default" }],
  ["preDeployCommand", { placeholder: "None" }],
  ["buildCommand", { placeholder: "Detected by Railpack" }],
  ["rootDir", { placeholder: "/apps/api", unset: "/" }],
]);

/** One-line hints for rows whose catalog description runs long; the rest use the catalog's words. */
// Most rows need none: the label and the placeholder say it. A hint only where it changes a decision.
const HINTS = new Map<ServiceSettingName, string | undefined>([
  ["replicas", undefined],
  ["cpuLimit", undefined],
  ["memLimit", undefined],
  ["startCommand", undefined],
  ["preDeployCommand", undefined],
  ["restartPolicy", undefined],
]);
const hint = (name: ServiceSettingName, setting: SettingSchema) => HINTS.has(name) ? HINTS.get(name) : setting.description;
/** What an unset limit means, shown in its empty field. */
const UNSET = new Map<ServiceSettingName, string>([["cpuLimit", "No limit"], ["memLimit", "No limit"]]);

const SUFFIXES = new Map<ServiceSettingName, string>([["replicas", "replicas"], ["memLimit", "GB"], ["cpuLimit", "vCPUs"]]);

/** What each restart policy does, shown under its choice. */
const OPTION_HELP = new Map([
  ["unless-stopped", "Restart unless you stop it."],
  ["always", "Restart whenever it stops."],
  ["on-failure", "Restart when it exits with an error, up to Max retries."],
  ["no", "Never restart."],
]);

function ServiceMonitoring({ state, namespace, ...props }: Omit<ComponentProps<typeof MonitoringTab>, "placedOn"> & { state: StoreService; namespace: string | null }) {
  const runtime = useServiceRuntime(state, namespace);
  return <MonitoringTab {...props} placedOn={new Set(runtime?.containers.map((container) => container.machineId)).size} />;
}

/** A Deployment Policy value as text; null when unset. */
const policyText = (value: JsonValue | undefined) => value === null || value === undefined ? null : String(value);

/** Saves one scalar Setting, and keeps a Store refusal (a rule the panel didn't foresee) to show on its row. */
function useSettingEdit(state: StoreService, name: ServiceSettingName) {
  const [refusal, setRefusal] = useState<string | null>(null);
  const setting = scalarSetting(name);
  const edit = (raw: string) => {
    setRefusal(null);
    const transaction = state.edit(settingChange(state.path(name), setting, raw));
    transaction.isPersisted.promise.catch((error: Error) => {
      if (error instanceof StoreRefused && error.code === "conflict") setRefusal(error.message);
    });
    return transaction;
  };
  return { setting, refusal, edit };
}

/** A number or text Setting's input, with its unit and what blank means. */
function StoreSettingInput({ state, name, setting, row, onCommit }: {
  state: StoreService; name: ServiceSettingName; setting: SettingSchema; row: SettingRow; onCommit: (raw: string) => Persistable;
}) {
  return (
    <ServiceSettingInput
      ariaLabel={setting.title}
      inputMode={setting.type === "integer" ? "numeric" : setting.type === "number" ? "decimal" : "text"}
      suffix={SUFFIXES.get(name)}
      placeholder={UNSET.get(name) ?? (settingText(setting.default) || undefined)}
      value={settingText(row.value)}
      {...changedProps(state.changes.get(name))}
      validate={(raw) => settingError(setting, raw)}
      onCommit={onCommit}
    />
  );
}

/** One scalar Setting as the catalog describes it: its title, help, bounds and choices. */
function StoreSettingField({ state, name }: { state: StoreService; name: ServiceSettingName }) {
  const { setting, refusal, edit } = useSettingEdit(state, name);
  const row = state.rows.get(name);
  const [current, hold] = useCommitHold(settingText(row?.value ?? row?.default));
  // Settings that don't apply to this source (a Dockerfile path on an image) have no row.
  if (!row) return null;
  const change = state.changes.get(name);

  if (setting.type === "boolean") {
    return (
      <SettingsRow label={setting.title} hint={hint(name, setting)} commit="change" control={
        <Switch id={`setting-${name}`} aria-label={setting.title} checked={current === "true"}
          onCheckedChange={(next) => hold(String(next), state.set(name, next))} />
      } />
    );
  }
  if (setting.type === "array") return <StoreListField state={state} name={name} setting={setting} row={row} />;
  const command = COMMANDS.get(name);
  if (command) {
    return (
      <ServiceCommandField label={setting.title} description={hint(name, setting)} placeholder={command.placeholder}
        value={row.value === null || settingText(row.value) === command.unset ? null : settingText(row.value)}
        {...changedProps(change)}
        validate={(raw) => settingError(setting, raw)} onCommit={(value) => edit(value === null || value === command.unset ? "" : value)} />
    );
  }

  // The input says a refused write itself, with Retry, so only the Select below needs the refusal here.
  if (!setting.enum) {
    return (
      <SettingsRow label={setting.title} hint={hint(name, setting)} commit="confirm"
        control={<StoreSettingInput state={state} name={name} setting={setting} row={row} onCommit={edit} />} />
    );
  }
  return (
    <SettingsRow label={setting.title} hint={hint(name, setting)} commit="change" control={
      <Select value={current} onValueChange={(next) => { if (next !== null && next !== current) hold(next, edit(next)); }}>
        <SelectTrigger aria-label={setting.title} className={OPTION_HELP.has(current) ? "py-1.5 data-[size=default]:h-auto" : undefined} data-changed={change ? true : undefined}
          title={change ? `Deployed: ${settingText(change.before)}` : undefined}>
          <SelectValue>
            <span className="flex flex-col items-start">
              <span>{OPTION_LABELS.get(current) ?? current}</span>
              {OPTION_HELP.has(current) ? <span className="text-xs text-muted-foreground">{OPTION_HELP.get(current)}</span> : null}
            </span>
          </SelectValue>
        </SelectTrigger>
        <SelectContent>
          <SelectGroup>
            {setting.enum.map((option) => (
              <SelectItem key={option} value={option} label={OPTION_LABELS.get(option) ?? option}>
                <span className="flex flex-col">
                  <span>{OPTION_LABELS.get(option) ?? option}</span>
                  {OPTION_HELP.has(option) ? <span className="text-xs text-muted-foreground">{OPTION_HELP.get(option)}</span> : null}
                </span>
              </SelectItem>
            ))}
          </SelectGroup>
        </SelectContent>
      </Select>
    }>
      {refusal ? <FieldError>{refusal}</FieldError> : null}
    </SettingsRow>
  );
}

/** Whether Setting `name` holds a value other than the one that means unset. */
const isSet = (state: StoreService, name: ServiceSettingName, unset?: string) => {
  const value = state.rows.get(name)?.value;
  if (value === null || value === undefined) return false;
  if (Array.isArray(value)) return value.length > 0;
  return settingText(value) !== unset;
};

/** A Setting most Services never set, behind its `+ X` until it holds a value. */
function StoreOptionalField({ state, name, addLabel, unset }: { state: StoreService; name: ServiceSettingName; addLabel: string; unset?: string }) {
  if (!state.rows.get(name)) return null;
  const setting = scalarSetting(name);
  return (
    <SettingsDisclosure label={setting.title} addLabel={addLabel} configured={isSet(state, name, unset)}>
      {() => <StoreSettingField state={state} name={name} />}
    </SettingsDisclosure>
  );
}

/** CPU and memory limits: one setting most Services never set, both off by one X. */
function StoreResourceLimits({ state }: { state: StoreService }) {
  const configured = isSet(state, "cpuLimit") || isSet(state, "memLimit");
  return (
    <SettingsDisclosure label="Resource limits" addLabel="Resource limits" configured={configured}>
      {(close) => (
        <SettingsGroup label="Resource limits" hint="Blank means no limit." commit="change" control={
          <Button type="button" variant="ghost" size="icon-sm" aria-label="Remove resource limits"
            onClick={() => {
              if (!configured) return close();
              if (isSet(state, "cpuLimit")) state.set("cpuLimit", null);
              if (isSet(state, "memLimit")) state.set("memLimit", null);
            }}>
            <XIcon />
          </Button>
        }>
          <StoreLimitField state={state} name="cpuLimit" />
          <StoreLimitField state={state} name="memLimit" />
        </SettingsGroup>
      )}
    </SettingsDisclosure>
  );
}

function StoreLimitField({ state, name }: { state: StoreService; name: ServiceSettingName }) {
  const { setting, edit } = useSettingEdit(state, name);
  const row = state.rows.get(name);
  if (!row) return null;
  return (
    <SettingsField label={setting.title} commit="confirm"
      control={<StoreSettingInput state={state} name={name} setting={setting} row={row} onCommit={edit} />} />
  );
}

/** A Git Service's Dockerfile, with the repository's Dockerfiles as suggestions. */
function StoreDockerfile({ state }: { state: StoreService }) {
  const repository = settingText(state.rows.get("repository")?.value);
  const gitRef = useRepositoryRef(state.organizationSlug, state.environment, repository);
  return (
    <StoreDockerfileField organizationSlug={state.organizationSlug} gitRef={gitRef} branch={settingText(state.rows.get("branch")?.value)}
      value={settingText(state.rows.get("dockerfilePath")?.value)} change={state.changes.get("dockerfilePath")}
      onCommit={(raw) => state.edit(settingChange(state.path("dockerfilePath"), serviceSetting("dockerfilePath"), raw))} />
  );
}

/** A list Setting, like watch paths: each entry a removable badge, one added at a time. */
function StoreListField({ state, name, setting, row }: { state: StoreService; name: ServiceSettingName; setting: SettingSchema; row: SettingRow }) {
  const [adding, setAdding] = useState("");
  const list = Schema.is(Schema.Array(Schema.String))(row.value) ? row.value : [];
  const change = state.changes.get(name);
  const save = (next: readonly string[]) => state.set(name, next.length ? [...next] : null);
  const add = () => {
    const next = adding.trim();
    if (next && !list.includes(next)) save([...list, next]);
    setAdding("");
  };
  return (
    <SettingsRow label={setting.title} hint={setting.description} commit="confirm" control={
      <div className="flex items-center gap-2">
        <Input aria-label={`New ${setting.title.toLowerCase()}`} placeholder="/src/**" value={adding}
          data-changed={change ? true : undefined} title={change ? `Deployed: ${settingText(change.before) || "none"}` : undefined}
          onChange={(event) => setAdding(event.target.value)}
          onKeyDown={(event) => { if (event.key === "Enter") { event.preventDefault(); add(); } }} />
        <Button type="button" variant="outline" onClick={add}><PlusIcon data-icon="inline-start" />Add</Button>
      </div>
    }>
      {list.length ? (
        <div className="flex flex-wrap gap-2">
          {list.map((entry) => (
            <Badge key={entry} variant="secondary">
              {entry}
              <button type="button" aria-label={`Remove ${entry}`} className="-mr-0.5 ml-1 rounded-sm opacity-70 hover:opacity-100"
                onClick={() => save(list.filter((other) => other !== entry))}>
                <XIcon className="size-3" />
              </button>
            </Badge>
          ))}
        </div>
      ) : null}
    </SettingsRow>
  );
}

type SourceKind = "image" | "repository";
const SOURCES = {
  repository: { label: "GitHub repository", icon: <GitHubMarkIcon /> },
  image: { label: "Container image", icon: <PackageIcon /> },
} satisfies Record<SourceKind, { label: string; icon: ReactNode }>;
const other = (kind: SourceKind): SourceKind => kind === "image" ? "repository" : "image";

/** A source the next deploy drops: what was deployed, struck through. */
function RemovedSource({ state, kind }: { state: StoreService; kind: SourceKind }) {
  const before = state.changes.get(kind)?.before;
  if (before === null || before === undefined || isSet(state, kind)) return null;
  return (
    <SettingsListItem media={SOURCES[kind].icon} label={settingText(before)} hint="Removed on your next deploy." tone="removed" commit="none" />
  );
}

/** Where the image comes from: a container image, or a repository with its branch and root directory. */
export function StoreSourceSection({ state }: { state: StoreService }) {
  const [picking, setPicking] = useState<{ kind: SourceKind; replacing: boolean } | null>(null);
  const githubWeb = useGithubWebOrigin();
  const set = (name: SourceKind | "branch", value: string | null) => state.set(name, value);
  const close = (open: boolean) => { if (!open) setPicking(null); };
  const kind: SourceKind = state.source === "git" ? "repository" : "image";
  // Staged: the Service is empty again once deployed; its settings for this source stay until then.
  const disconnect = () => state.set(kind, null);
  const setting = serviceSetting(kind);
  const [value, hold] = useCommitHold(settingText(state.rows.get(kind)?.value));
  const change = state.changes.get(kind);
  // Its parts change as one row, so what was deployed is the whole source.
  const deployed = `Deployed: ${sourceText(state.changes.get("source")?.before ?? null) || "none"}`;
  const gitRef = useRepositoryRef(state.organizationSlug, state.environment, value);

  if (state.source === "uploaded") {
    return (
      <SettingsRow label="Uploaded" commit="none"
        hint={<>Uploaded from a directory with <code>ployz up</code>; run it again there to ship new code.</>} />
    );
  }

  // A pick replaces the source, so switching kinds drops the old one in the same breath.
  const pick = (picked: SourceKind, next: string) => {
    const replacing = picking?.replacing === true;
    setPicking(null);
    if (picked === kind || state.source === "empty") hold(next, set(picked, next));
    else set(picked, next);
    if (replacing) set(other(picked), null);
  };
  const dialogs = <>
    <ImageSelectorDialog open={picking?.kind === "image"} onOpenChange={close} onSelectImage={(image) => pick("image", image)} />
    <GitRepoSelectorDialog open={picking?.kind === "repository"} onOpenChange={close} organizationSlug={state.organizationSlug}
      onSelectRepo={({ fullName }) => pick("repository", fullName)} />
  </>;

  if (state.source === "empty") {
    const removed = (["repository", "image"] as const).filter((candidate) => state.changes.get(candidate)?.before != null);
    return <>
      {removed.length ? (
        <SettingsList label="Source">
          {removed.map((candidate) => <RemovedSource key={candidate} state={state} kind={candidate} />)}
        </SettingsList>
      ) : null}
      <SettingsEmpty title="No source">A deploy without a source fails.</SettingsEmpty>
      <SettingsFoot>
        {(["repository", "image"] as const).map((candidate) => (
          <SettingsAddButton key={candidate} onClick={() => setPicking({ kind: candidate, replacing: false })}>{SOURCES[candidate].label}</SettingsAddButton>
        ))}
      </SettingsFoot>
      {dialogs}
    </>;
  }

  const connecting = change !== undefined && change.before === null;
  return <>
    <SettingsList label="Source">
      <RemovedSource state={state} kind={other(kind)} />
      <SettingsListItem media={SOURCES[kind].icon}
        label={kind === "repository"
          ? <a className="truncate hover:underline" href={`${githubWeb}/${value}`} target="_blank" rel="noreferrer" title={change ? deployed : undefined}>{value}</a>
          : <span className="truncate font-mono" title={change ? deployed : undefined}>{value}</span>}
        hint={connecting ? "Connects on your next deploy." : state.service.template ? `From the ${templateLabel(state.service.template)} template.` : undefined}
        tone={connecting ? "new" : change ? "changed" : undefined}
        commit="change"
        actions={<>
          <Button type="button" variant="ghost" size="icon-sm" aria-label={`Change ${setting.title.toLowerCase()}`}
            onClick={() => setPicking({ kind, replacing: false })}>
            <PencilIcon />
          </Button>
          <DropdownMenu>
            <DropdownMenuTrigger render={<Button type="button" variant="ghost" size="icon-sm" aria-label="Source actions" />}>
              <EllipsisIcon />
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              <DropdownMenuGroup>
                <DropdownMenuItem onClick={() => setPicking({ kind: other(kind), replacing: true })}>
                  Switch to {SOURCES[other(kind)].label.toLowerCase()}
                </DropdownMenuItem>
              </DropdownMenuGroup>
              <DropdownMenuSeparator />
              <DropdownMenuGroup>
                <DropdownMenuItem variant="destructive" onClick={disconnect}>Disconnect</DropdownMenuItem>
              </DropdownMenuGroup>
            </DropdownMenuContent>
          </DropdownMenu>
        </>} />
    </SettingsList>
    {kind === "repository" ? (
      <>
        <StoreBranchField organizationSlug={state.organizationSlug} repository={value} gitRef={gitRef} value={settingText(state.rows.get("branch")?.value)}
          change={state.changes.get("branch")} onSet={(branch) => set("branch", branch)} />
        <StoreOptionalField state={state} name="rootDir" addLabel="Root directory" unset="/" />
        {/* Its Deployment Policy: when a push to the branch deploys. */}
        <StoreSettingField state={state} name="autoDeploy" />
        <StoreSettingField state={state} name="waitForCi" />
        <StoreOptionalField state={state} name="watchPaths" addLabel="Watch paths" />
      </>
    ) : <StoreRegistryCredentials state={state} image={value} />}
    {dialogs}
  </>;
}

/**
 * A private image's pull credentials. A new secret replaces the stored one at once; turning them off is staged and
 * keeps the stored secret, which Restore turns back on. Reads show only that there is one.
 */
function StoreRegistryCredentials({ state, image }: { state: StoreService; image: string }) {
  const row = state.rows.get("registryCredential");
  const [configured, hold] = useCommitHold(row !== undefined && row.value !== null);
  if (!row) return null;
  const change = state.changes.get("registryCredential");
  const shown = (value: JsonValue) => value === null ? "None" : "Configured";
  return (
    <RegistryCredentialsField
      label={serviceSetting("registryCredential").title}
      image={image}
      configured={configured}
      username={null}
      changed={change !== undefined}
      baselineValue={change ? shown(change.before) : undefined}
      onSet={({ username, secret }) => {
        const write = state.set("registryCredential", username === null ? { secret } : { username, secret });
        hold(true, write);
        return write;
      }}
      onClear={() => hold(false, state.set("registryCredential", null))}
      onRestore={!configured && change?.before != null ? () => hold(true, state.set("registryCredential", { secret: true })) : undefined}
    />
  );
}

function StoreDangerSection({ state }: { state: StoreService }) {
  const remove = useRemoveStoreService(state.environment, state.service);
  return (
    <SettingsDanger label="Delete this service" hint="The service is removed on your next deploy."
      action={(
        <Button variant="destructive" onClick={remove}>
          <Trash2Icon data-icon="inline-start" />
          Delete service
        </Button>
      )} />
  );
}

/** Replicas while a volume without shared writes is attached: one, fixed, and the way to allow more. */
function StoreReplicasCapped({ params, volume }: {
  params: { organizationSlug: string; projectSlug: string; environmentSlug: string };
  volume: VolumeListing;
}) {
  return (
    <SettingsRow label="Replicas" commit="none"
      hint={<>
        Limited to 1 while <strong>{volume.name}</strong> is attached.{" "}
        <span className="inline-flex align-middle"><InfoHint>{SHARED_VOLUME_WHY}</InfoHint></span>{" "}
        <Link className="whitespace-nowrap" to={ENVIRONMENT_RESOURCE_ROUTE_TO} params={{ ...params, resourceId: volume.id }}>Allow shared writes</Link>
      </>}
      control={<Input id="replicas-capped" aria-label="Replicas" value="1" disabled />} />
  );
}
