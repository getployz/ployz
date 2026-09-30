import { Suspense, useState, type ReactNode } from "react";
import { useLoaderData, useNavigate, useSearch } from "@tanstack/react-router";
import { Schema } from "effect";
import { PackageIcon, PencilIcon, PlusIcon, Trash2Icon, XIcon } from "lucide-react";
import type { Change, EnvironmentRef, JsonValue, ServiceListing, ServiceSettingChange, SettingRow } from "@ployz/sdk";
import { GitRepoSelectorDialog, ImageSelectorDialog } from "#/components/service-source-selector";
import { GitHubMarkIcon } from "#/components/icons/github-mark";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Input } from "#/components/ui/input";
import { Field, FieldDescription, FieldGroup, FieldLabel } from "#/components/ui/field";
import { Item, ItemActions, ItemContent, ItemDescription, ItemMedia, ItemTitle } from "#/components/ui/item";
import { shownValue } from "#/modules/config-store/store-deployments";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "#/components/ui/tabs";
import { Select, SelectContent, SelectGroup, SelectItem, SelectTrigger, SelectValue } from "#/components/ui/select";
import { serviceSetting, settingChange, settingError, type ServiceSettingName, type SettingSchema } from "#/modules/config-store/catalog";
import { templateLabel } from "#/modules/config-store/database-presets";
import { changedProps, dnsLabelError, serviceChanges, serviceSettingRows, settingText } from "#/modules/config-store/store-services";
import { diffQuery, environmentSettingsQuery, requireView, servicesQuery, useStoreViews } from "#/modules/config-store/store-view.queries";
import type { Persistable } from "#/collections/query-collection";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { CanvasInspectorHeader } from "../../../-components/CanvasInspectorHeader";
import { CanvasInspectorNotFound } from "../../../-components/CanvasInspectorRouteStates";
import { CanvasInspectorNameEditor } from "../../../-components/CanvasInspectorNameEditor";
import { ENVIRONMENT_ROUTE_FROM } from "../../../-components/environment-route-paths";
import { RegistryCredentialsField } from "./ServiceRegistryCredentialsSection";
import { ServiceSettingInput } from "./ServiceSettingInput";
import { ServiceCommandField } from "./ServiceCommandField";
import { StoreBranchField, StoreDockerfileField, StorePreferredBuilderField, useRepositoryRef } from "./StoreGitFields";
import { SwitchField } from "../../../-components/branch-review/SaveSheet";
import { ServiceSettingsSection } from "./ServiceSettingsSection";
import { SERVICE_SETTINGS_SECTIONS, type ServiceSettingsSectionId } from "./service-settings-sections";
import { useRemoveStoreService } from "./useDeleteService";
import { StoreServiceVariablesTab } from "./ServiceVariablesTab";
import { StoreNetworkingSection } from "./StoreNetworkingSection";
import { ContainerLogs } from "#/components/container-logs";
import { Skeleton } from "#/components/ui/skeleton";
import { ItemGroup } from "#/components/ui/item";
import { StoreDeploymentRows } from "../../../-components/DeploymentsList";
import { SERVICE_PAGES, servicePageSchema } from "./service-pages";

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

/** A DNS label no other Service here has as its name or Private DNS. */
function nameSchema(service: ServiceListing, services: readonly ServiceListing[]) {
  return Schema.String.check(Schema.makeFilter<string>((name) =>
    dnsLabelError(name) ?? (taken(service, services, name) ? `A service here is already named ${name}.` : undefined)));
}

/**
 * A Service's drawer over the Config Store: its name, source and scalar Settings, labelled by the catalog. Edits save
 * through the Environment's queue; a rename or removal is staged like any other change.
 */
export function StoreServiceDrawer({ params }: { params: { organizationSlug: string; projectSlug: string; environmentSlug: string; serviceId: string } }) {
  const { store } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const { organizationSlug } = params;
  const views = useStoreViews(organizationSlug, [servicesQuery(store), environmentSettingsQuery(store), diffQuery(store)] as const);
  const services = requireView(views[0]).services;
  const settings = requireView(views[1]);
  const diff = requireView(views[2]);
  const writer = useStoreWriter(organizationSlug);
  const { tab } = useSearch({ from: SERVICE_ROUTE_FROM });
  const navigate = useNavigate({ from: SERVICE_ROUTE_TO });
  const service = services.find((candidate) => candidate.id === params.serviceId);
  // A stale link, or removed while open (from the CLI; the drawer's own Delete closes it).
  if (!service) return <CanvasInspectorNotFound noun="Service" />;

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
  const rename = state.changes.get("name");
  const restartPolicy = state.rows.get("restartPolicy");
  const buildMethod = settingText(state.rows.get("buildMethod")?.value ?? state.rows.get("buildMethod")?.default);
  const field = (name: ServiceSettingName) => <StoreSettingField key={name} state={state} name={name} />;
  const bodies = {
    source: <StoreSourceSection state={state} />,
    // Domain statuses need a look at the Cluster, so the rest of the drawer doesn't wait for them.
    networking: (
      <Suspense fallback={null}>
        <StoreNetworkingSection state={state} version={diff.version}
          validatePrivateDns={(raw) => raw === "" ? null : settingError(serviceSetting("privateDns"), raw)
            ?? (taken(service, services, raw) ? `A service here is already reached as ${raw}.` : null)} />
      </Suspense>
    ),
    scale: <FieldGroup>{field("replicas")}{field("cpuLimit")}{field("memLimit")}</FieldGroup>,
    build: state.source === "git" ? (
      <FieldGroup>
        {field("buildMethod")}
        {buildMethod === "dockerfile" ? <StoreDockerfile state={state} /> : field("buildCommand")}
        <StorePreferredBuilderField organizationSlug={organizationSlug} value={policyText(state.rows.get("preferredBuilder")?.value)}
          onSet={(builder) => state.set("preferredBuilder", builder)} />
      </FieldGroup>
    ) : null,
    deploy: (
      <FieldGroup>
        {field("startCommand")}
        {field("preDeployCommand")}
        <StoreHealthcheckField state={state} />
        {field("restartPolicy")}
        {(restartPolicy?.value ?? restartPolicy?.default) === "on-failure" ? field("maxRetries") : null}
      </FieldGroup>
    ),
    danger: <StoreDangerSection state={state} />,
  } satisfies Record<ServiceSettingsSectionId, ReactNode>;

  return (
    <div className="flex h-full min-h-0 flex-col">
      <CanvasInspectorHeader params={params}>
        <CanvasInspectorNameEditor
          value={service.name}
          schema={nameSchema(service, services)}
          editTitle="Edit service name"
          editDescription="Rename this service. Other services reach it at its Private DNS name, which stays."
          placeholder="Service name"
          {...changedProps(rename)}
          onRename={(name) => writer.commit({ command: "rename_service", environment: store, service: service.name, name })}
        />
      </CanvasInspectorHeader>
      <div className="flex min-h-0 flex-1 flex-col px-4 pb-4">
        <Tabs value={Schema.is(servicePageSchema)(tab) ? tab : "settings"}
          onValueChange={(value) => {
            if (Schema.is(servicePageSchema)(value)) void navigate({ search: (prev) => ({ ...prev, tab: value }), replace: true });
          }}
          className="flex min-h-0 flex-1 flex-col overflow-clip">
          <TabsList variant="line" className="max-w-full shrink-0 overflow-x-auto">
            {SERVICE_PAGES.map((page) => <TabsTrigger key={page.id} value={page.id}>{page.label}</TabsTrigger>)}
          </TabsList>
          <TabsContent value="deployments" className="mt-4 min-h-0 flex-1 overflow-y-auto">
            <nav aria-label="Deployments" className="mx-auto w-full max-w-2xl">
              <ItemGroup className="gap-1">
                <Suspense fallback={<Skeleton className="h-16 w-full" />}>
                  <StoreDeploymentRows service={service} returnTo={service.id} />
                </Suspense>
              </ItemGroup>
            </nav>
          </TabsContent>
          <TabsContent value="logs" className="mt-4 flex min-h-0 flex-1 flex-col">
            <ContainerLogs selection={{ organizationSlug, projectSlug: params.projectSlug, environmentSlug: params.environmentSlug, serviceId: service.id }} />
          </TabsContent>
          <TabsContent value="settings" className="mt-3 min-h-0 flex-1 overflow-clip">
            <div className="h-full overflow-y-auto pr-1 pb-8">
              <div className="mx-auto flex w-full max-w-2xl flex-col gap-6 **:data-[slot=field-group]:gap-4">
                {SERVICE_SETTINGS_SECTIONS.flatMap((section) => {
                  const body = bodies[section.id];
                  return body ? [(
                    <ServiceSettingsSection key={section.id} id={section.id} title={section.label} description={section.description}
                      variant={section.id === "danger" ? "danger" : "default"}>
                      {body}
                    </ServiceSettingsSection>
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

/**
 * Optional Settings shown as a button until set: commands, and the root directory (whose `/` is the default). Pre-deploy
 * and the root directory are ones most Services never need.
 */
const COMMANDS = new Map<ServiceSettingName, { placeholder: string; compact: boolean; addLabel?: string; unset?: string }>([
  ["startCommand", { placeholder: "npm start", compact: false }],
  ["preDeployCommand", { placeholder: "npm run migrate", compact: true }],
  ["buildCommand", { placeholder: "pnpm run build", compact: false }],
  ["rootDir", { placeholder: "/apps/api", compact: true, addLabel: "Add root directory", unset: "/" }],
]);

/** Suffixes after a number, as before the Store: "3 replicas". */
const SUFFIXES = new Map<ServiceSettingName, string>([["replicas", "replicas"], ["memLimit", "GB"], ["cpuLimit", "vCPUs"]]);

/** What each restart policy does, shown under its choice. */
const OPTION_HELP = new Map([
  ["unless-stopped", "Restart unless you stop it."],
  ["always", "Restart whenever it stops."],
  ["on-failure", "Restart when it exits with an error, up to Max retries."],
  ["no", "Never restart."],
]);

/** A Deployment Policy value as text; null when unset. */
const policyText = (value: JsonValue | undefined) => value === null || value === undefined ? null : String(value);

/** One scalar Setting as the catalog describes it: its title, help, bounds and choices. */
function StoreSettingField({ state, name }: { state: StoreService; name: ServiceSettingName }) {
  const setting: SettingSchema = serviceSetting(name);
  const row = state.rows.get(name);
  // Settings that don't apply to this source (a Dockerfile path on an image) have no row.
  if (!row) return null;
  const change = state.changes.get(name);
  const current = settingText(row.value ?? row.default);
  const edit = (raw: string) => state.edit(settingChange(state.path(name), setting, raw));

  if (setting.type === "boolean") {
    const on = (row.value ?? row.default) === true;
    return (
      <SwitchField id={`setting-${name}`} label={setting.title} description={setting.description} checked={on}
        onChange={(next) => state.set(name, next)} />
    );
  }
  if (setting.type === "array") return <StoreListField state={state} name={name} setting={setting} row={row} />;
  const command = COMMANDS.get(name);
  if (command) {
    return (
      <ServiceCommandField label={setting.title} addLabel={command.addLabel} description={setting.description} placeholder={command.placeholder}
        compact={command.compact} value={row.value === null || settingText(row.value) === command.unset ? null : settingText(row.value)}
        {...changedProps(change)}
        validate={(raw) => settingError(setting, raw)} onCommit={(value) => edit(value === null || value === command.unset ? "" : value)} />
    );
  }

  return (
    <Field>
      <FieldLabel>{setting.title}</FieldLabel>
      <FieldDescription>{setting.description}</FieldDescription>
      {setting.enum ? (
        <Select value={current} onValueChange={(next) => { if (next !== null && next !== current) edit(next); }}>
          <SelectTrigger aria-label={setting.title} className={OPTION_HELP.has(current) ? "w-full py-1.5 data-[size=default]:h-auto" : "w-full"} data-changed={change ? true : undefined}
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
      ) : (
        <ServiceSettingInput
          ariaLabel={setting.title}
          type={setting.type === "string" ? "text" : "number"}
          inputMode={setting.type === "integer" ? "numeric" : setting.type === "number" ? "decimal" : "text"}
          min={setting.minimum}
          max={setting.maximum}
          step={setting.type === "integer" ? 1 : "any"}
          suffix={SUFFIXES.get(name)}
          placeholder={settingText(setting.default) || undefined}
          value={settingText(row.value)}
          {...changedProps(change)}
          validate={(raw) => settingError(setting, raw)}
          onCommit={edit}
        />
      )}
    </Field>
  );
}

const Healthcheck = Schema.Struct({ path: Schema.String, timeoutSeconds: Schema.Number });
/** The first staged change among `names`, as the drawer keys them. */
const changeOf = (changes: Map<string, ServiceSettingChange>, ...names: string[]) =>
  names.map((name) => changes.get(name)).find((change) => change !== undefined);

/**
 * The HTTP check a new replica passes before it takes traffic: a path (a button until set; blank turns it off) and,
 * once on, how long it may take.
 */
function StoreHealthcheckField({ state }: { state: StoreService }) {
  const row = state.rows.get("healthcheck");
  if (!row) return null;
  const setting = serviceSetting("healthcheck");
  const { path: pathSchema, timeoutSeconds } = setting.properties;
  const on = Schema.is(Healthcheck)(row.value) ? row.value : null;
  const pathChange = changeOf(state.changes, "healthcheck.path", "healthcheck");
  const timeoutChange = changeOf(state.changes, "healthcheck.timeoutSeconds", "healthcheck");
  const shown = (value: JsonValue | undefined) => Schema.is(Healthcheck)(value) ? `${value.path}, ${value.timeoutSeconds}s`
    : value === null || value === undefined ? "off" : settingText(value);
  return (
    <>
      <ServiceCommandField label={setting.title} addLabel="Healthcheck path" description={setting.description} placeholder="/up"
        value={on?.path ?? null} {...changedProps(pathChange, shown)}
        validate={(raw) => settingError({ ...pathSchema, title: "path", description: "", type: "string" }, raw)
          ?? (raw.startsWith("/") ? null : "Start the path with /.")}
        onCommit={(next) => state.set("healthcheck", next)} />
      {on ? (
        <Field>
          <FieldLabel>Healthcheck timeout</FieldLabel>
          <FieldDescription>How long a new replica may take to pass it.</FieldDescription>
          <ServiceSettingInput ariaLabel="Healthcheck timeout" type="number" inputMode="numeric" min={timeoutSeconds.minimum}
            max={timeoutSeconds.maximum} step={1} suffix="seconds" placeholder={String(timeoutSeconds.default)}
            value={String(on.timeoutSeconds)} {...changedProps(timeoutChange, shown)}
            validate={(raw) => settingError({ ...timeoutSeconds, title: "timeout", description: "", type: "integer" }, raw)}
            onCommit={(raw) => state.set("healthcheck", { path: on.path, timeoutSeconds: raw === "" ? timeoutSeconds.default : Number(raw) })} />
        </Field>
      ) : null}
    </>
  );
}

/** A Git Service's Dockerfile, with the repository's Dockerfiles as suggestions. */
function StoreDockerfile({ state }: { state: StoreService }) {
  const repository = settingText(state.rows.get("repository")?.value);
  const gitRef = useRepositoryRef(state.organizationSlug, state.environment, repository);
  return (
    <StoreDockerfileField gitRef={gitRef} branch={settingText(state.rows.get("branch")?.value)}
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
    <Field>
      <FieldLabel>{setting.title}</FieldLabel>
      <FieldDescription>{setting.description}</FieldDescription>
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
      <div className="flex items-center gap-2">
        <Input aria-label={`New ${setting.title.toLowerCase()}`} placeholder="/src/**" className="flex-1" value={adding}
          data-changed={change ? true : undefined} title={change ? `Deployed: ${settingText(change.before) || "none"}` : undefined}
          onChange={(event) => setAdding(event.target.value)}
          onKeyDown={(event) => { if (event.key === "Enter") { event.preventDefault(); add(); } }} />
        <Button type="button" variant="outline" onClick={add}><PlusIcon data-icon="inline-start" />Add</Button>
      </div>
    </Field>
  );
}

/** Where the image comes from: a container image, or a repository with its branch and root directory. */
function StoreSourceSection({ state }: { state: StoreService }) {
  const [picking, setPicking] = useState<"image" | "repository" | null>(null);
  const set = (name: "image" | "repository" | "branch", value: string) => state.set(name, value);
  const close = (open: boolean) => { if (!open) setPicking(null); };
  const kind = state.source === "git" ? "repository" : "image";
  // Staged: the Service is empty again once deployed; its settings for this source stay until then.
  const disconnect = () => state.set(kind, null);
  const setting = serviceSetting(kind);
  const value = settingText(state.rows.get(kind)?.value);
  const change = state.changes.get(kind);
  const gitRef = useRepositoryRef(state.organizationSlug, state.environment, value);

  if (state.source === "uploaded") {
    return (
      <Field>
        <FieldLabel>Source</FieldLabel>
        <FieldDescription>Uploaded from a directory with <code>ployz up</code>; run it again there to ship new code.</FieldDescription>
      </Field>
    );
  }

  const dialogs = <>
    <ImageSelectorDialog open={picking === "image"} onOpenChange={close} onSelectImage={(image) => { set("image", image); setPicking(null); }} />
    <GitRepoSelectorDialog open={picking === "repository"} onOpenChange={close}
      onSelectRepo={({ fullName }) => { set("repository", fullName); setPicking(null); }} />
  </>;

  if (state.source === "empty") {
    const change = changeOf(state.changes, "source", "image", "repository");
    return (
      <FieldGroup>
        {change ? (
          <Item variant="muted" size="sm" data-changed>
            <ItemContent>
              <ItemTitle>No source after your next deploy</ItemTitle>
              <ItemDescription>Deployed: {shownValue(change.before) || "none"}</ItemDescription>
            </ItemContent>
          </Item>
        ) : null}
        <Field>
          <FieldLabel>Add a source</FieldLabel>
          <FieldDescription>Deploy from a GitHub repository, or run a container image.</FieldDescription>
          <div className="flex flex-wrap gap-2">
            <Button type="button" variant="outline" onClick={() => setPicking("repository")}><GitHubMarkIcon data-icon="inline-start" />Git repository</Button>
            <Button type="button" variant="outline" onClick={() => setPicking("image")}><PackageIcon data-icon="inline-start" />Container image</Button>
          </div>
        </Field>
        {dialogs}
      </FieldGroup>
    );
  }

  return (
    <FieldGroup>
      <Field>
        <FieldLabel>{setting.title}</FieldLabel>
        <FieldDescription>{setting.description}</FieldDescription>
        <Item variant="muted" data-changed={change ? true : undefined} title={change ? `Deployed: ${settingText(change.before)}` : undefined}>
          <ItemMedia variant="icon">{kind === "repository" ? <GitHubMarkIcon /> : <PackageIcon />}</ItemMedia>
          <ItemContent>
            <ItemTitle>{kind === "repository" ? <a href={`https://github.com/${value}`} target="_blank" rel="noreferrer">{value}</a> : value}</ItemTitle>
            {state.service.template ? <ItemDescription>From template: {templateLabel(state.service.template)}</ItemDescription> : null}
          </ItemContent>
          <ItemActions>
            <Button type="button" variant="ghost" size="icon" onClick={() => setPicking(kind)}>
              <PencilIcon />
              <span className="sr-only">Edit {setting.title.toLowerCase()}</span>
            </Button>
            <Button type="button" variant="ghost" size="sm" onClick={disconnect}>Disconnect</Button>
          </ItemActions>
        </Item>
      </Field>
      {kind === "repository" ? (
        <>
          <StoreBranchField repository={value} gitRef={gitRef} value={settingText(state.rows.get("branch")?.value)}
            change={state.changes.get("branch")} onSet={(branch) => set("branch", branch)} />
          <StoreSettingField state={state} name="rootDir" />
          {/* Its Deployment Policy: when a push to the branch deploys. */}
          <StoreSettingField state={state} name="autoDeploy" />
          <StoreSettingField state={state} name="waitForCi" />
          <StoreSettingField state={state} name="watchPaths" />
        </>
      ) : <StoreRegistryCredentials state={state} image={value} />}
      {dialogs}
    </FieldGroup>
  );
}

/**
 * A private image's pull credentials. A new secret replaces the stored one at once; turning them off is staged and
 * keeps the stored secret, which Restore turns back on. Reads show only that there is one.
 */
function StoreRegistryCredentials({ state, image }: { state: StoreService; image: string }) {
  const row = state.rows.get("registryCredential");
  if (!row) return null;
  const change = state.changes.get("registryCredential");
  const configured = row.value !== null;
  const shown = (value: JsonValue) => value === null ? "None" : "Configured";
  return (
    <RegistryCredentialsField
      label={serviceSetting("registryCredential").title}
      image={image}
      configured={configured}
      username={null}
      changed={change !== undefined}
      baselineValue={change ? shown(change.before) : undefined}
      onSet={({ username, secret }) => state.set("registryCredential", username === null ? { secret } : { username, secret })}
      onClear={() => state.set("registryCredential", null)}
      onRestore={!configured && change?.before != null ? () => state.set("registryCredential", { secret: true }) : undefined}
    />
  );
}

function StoreDangerSection({ state }: { state: StoreService }) {
  const remove = useRemoveStoreService(state.environment, state.service.name);
  return (
    <div className="flex flex-col items-start justify-between gap-4 rounded-xl border border-destructive-border bg-destructive-soft p-4 sm:flex-row sm:items-center">
      <div className="min-w-0">
        <div className="text-sm font-semibold text-destructive">Delete this service</div>
        <p className="mt-1 text-sm text-destructive/85">Deleted on your next deploy.</p>
      </div>
      <Button variant="destructive" className="shrink-0" onClick={remove}>
        <Trash2Icon data-icon="inline-start" />
        Delete service
      </Button>
    </div>
  );
}
