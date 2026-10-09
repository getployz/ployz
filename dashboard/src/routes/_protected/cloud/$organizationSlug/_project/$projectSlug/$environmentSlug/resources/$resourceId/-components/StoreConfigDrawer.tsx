import { useRef, useState } from "react";
import { Schema } from "effect";
import { useBlocker, useLoaderData, useNavigate } from "@tanstack/react-router";
import { EllipsisIcon, LockIcon, PlusIcon, Trash2Icon } from "lucide-react";
import type { ConfigItemView, ConfigListing, EnvironmentRef, ServiceListing } from "@ployz/sdk";
import {
  AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader,
  AlertDialogTitle,
} from "#/components/ui/alert-dialog";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import {
  DropdownMenu, DropdownMenuCheckboxItem, DropdownMenuContent, DropdownMenuGroup, DropdownMenuItem, DropdownMenuLabel, DropdownMenuTrigger,
} from "#/components/ui/dropdown-menu";
import { Empty, EmptyDescription } from "#/components/ui/empty";
import { Field, FieldError, FieldLabel } from "#/components/ui/field";
import { Input } from "#/components/ui/input";
import { Item, ItemActions, ItemContent, ItemDescription, ItemTitle } from "#/components/ui/item";
import { Select, SelectContent, SelectGroup, SelectItem, SelectTrigger, SelectValue } from "#/components/ui/select";
import { Skeleton } from "#/components/ui/skeleton";
import { ToggleGroup, ToggleGroupItem } from "#/components/ui/toggle-group";
import { cn } from "#/lib/utils";
import { configReferences, previewSegments } from "#/modules/config-store/config-references";
import {
  attachConfigCommand, configBytesText, configEdits, configFileSizeError, configReferenceValues, EXECUTABLE_MODE, fileAccess, READ_ONLY_MODE,
  saveConfigCommand, utf8Bytes, type FileDraft,
} from "#/modules/config-store/store-configs";
import { changedProps, dnsLabelError } from "#/modules/config-store/store-services";
import { storeReferenceTargets } from "#/modules/config-store/store-variables";
import { configQuery, configsQuery, diffQuery, environmentSettingsQuery, requireView, servicesQuery, useStoreViews } from "#/modules/config-store/store-view.queries";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { StoreRefused } from "#/modules/config-store/store.contract";
import type { ReferenceTarget } from "#/modules/variables/variable-autocomplete";
import { SettingsSection } from "#/routes/_protected/cloud/$organizationSlug/-components/SettingsSection";
import { DangerRow } from "#/routes/_protected/cloud/$organizationSlug/-components/danger-row";
import { CanvasInspectorHeader } from "../../../-components/CanvasInspectorHeader";
import { CanvasInspectorNameEditor } from "../../../-components/CanvasInspectorNameEditor";
import { CanvasInspectorNotFound } from "../../../-components/CanvasInspectorRouteStates";
import { useStoreChangeActions } from "../../../-components/canvas/useStoreChangeActions";
import { DEPLOYMENT_PAGE_ROUTE_TO } from "../../../-components/deployment-page";
import { useConfigFileEditor } from "./config-file-editor-loader";
import { ENVIRONMENT_INDEX_ROUTE_TO, ENVIRONMENT_ROUTE_FROM } from "../../../-components/environment-route-paths";


const SHOWN_REFUSALS = ["invalid", "conflict"] as const;

type ConfigRouteParams = { organizationSlug: string; projectSlug: string; environmentSlug: string; resourceId: string };
type StoreConfig = { organizationSlug: string; environment: EnvironmentRef; config: ConfigListing };

export function StoreConfigDrawer({ params, config }: { params: ConfigRouteParams; config: ConfigListing }) {
  const { store } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const { organizationSlug } = params;
  const views = useStoreViews(organizationSlug,
    [configsQuery(store), servicesQuery(store), diffQuery(store), environmentSettingsQuery(store), configQuery(store, config.name)] as const);
  const configs = requireView(views[0]).configs;
  const services = requireView(views[1]).services;
  const diff = requireView(views[2]);
  const settings = requireView(views[3]);
  const writer = useStoreWriter(organizationSlug);
  if (!views[4].ok) return <CanvasInspectorNotFound noun="Config" />;
  const item = views[4].value;
  const state: StoreConfig = { organizationSlug, environment: store, config };
  const removing = config.change === "delete";
  const renamed = diff.changes.find((change) => change.type === "config" && change.id === config.id)
    ?.settings.find((row) => row.path === "name" || row.path.endsWith(".name"));
  const nameSchema = Schema.String.check(Schema.makeFilter<string>((name) => dnsLabelError(name)
    ?? (configs.some((other) => other.id !== config.id && other.name === name) ? `A config here is already named ${name}.` : undefined)));
  const targets = storeReferenceTargets(settings, services, "");
  const values = configReferenceValues(settings, services);
  const serviceNames = services.map((service) => service.name);

  return (
    <div className="flex h-full min-h-0 flex-col">
      <CanvasInspectorHeader params={params}>
        {removing ? <p className="truncate font-semibold">{config.name}</p> : (
          <CanvasInspectorNameEditor value={config.name} schema={nameSchema} editTitle="Edit config name"
            editDescription="Rename this config. Its files and mounts stay." placeholder="Config name"
            {...changedProps(renamed)}
            onRename={(name) => writer.commit({ command: "rename_config", environment: store, config: config.name, name })} />
        )}
        <p className="truncate text-sm text-muted-foreground">
          {[`${config.files.length} file${config.files.length === 1 ? "" : "s"}`, config.deployed ? null : "not deployed",
            config.mounts.length ? `mounted on ${config.mounts.map((mount) => mount.service).join(", ")}` : "not mounted"]
            .filter(Boolean).join(" · ")}
        </p>
      </CanvasInspectorHeader>
      <ConfigBody key={config.id} state={state} item={item} services={services} targets={targets} serviceNames={serviceNames}
        values={values} removing={removing} params={params} version={diff.version} />
    </div>
  );
}

function ConfigBody({ state, item, services, targets, serviceNames, values, removing, params, version }: {
  state: StoreConfig; item: ConfigItemView; services: readonly ServiceListing[]; targets: readonly ReferenceTarget[];
  serviceNames: readonly string[]; values: ReturnType<typeof configReferenceValues>; removing: boolean;
  params: ConfigRouteParams; version: string;
}) {
  const writer = useStoreWriter(state.organizationSlug);
  const [drafts, setDrafts] = useState<ReadonlyMap<string, FileDraft>>(new Map());
  const [saveError, setSaveError] = useState<string | null>(null);
  const [failedSaves, setFailedSaves] = useState<ReadonlySet<string>>(new Set());
  const [pendingSaves, setPendingSaves] = useState<ReadonlySet<ReadonlyMap<string, FileDraft>>>(new Set());
  const submittedFiles = useRef(new Map<string, ReadonlyMap<string, FileDraft>>());
  const stored = item.files.map((file) => file.name);
  const fileNames = [...stored, ...[...drafts.keys()].filter((name) => !stored.includes(name))];
  const [selected, setSelected] = useState<string | null>(fileNames[0] ?? null);
  const current = selected !== null && fileNames.includes(selected) ? selected : fileNames[0] ?? null;
  const textOf = (file: string) => drafts.get(file)?.content ?? item.contents[file] ?? "";
  const edits = configEdits(item, drafts);
  // A failed rollback read can leave the optimistic contents cached.
  edits.push(...[...drafts]
    .filter(([file]) => failedSaves.has(file) && !edits.some((edit) => edit.file === file))
    .map(([file, draft]) => ({ file, ...draft })));
  const dirty = new Set(edits.map((edit) => edit.file));
  const unsaved = new Set([...dirty, ...[...pendingSaves].flatMap((submitted) => [...submitted.keys()])]);
  const oversized = fileNames.map((file) => ({ file, error: configFileSizeError(utf8Bytes(textOf(file))) }))
    .find((one) => one.error !== null);

  const blocker = useBlocker({
    shouldBlockFn: ({ current: from, next }) => unsaved.size > 0 && from.pathname !== next.pathname,
    enableBeforeUnload: () => unsaved.size > 0,
    withResolver: true,
  });

  function draft(file: string, change: Partial<FileDraft>) {
    setSaveError(null);
    submittedFiles.current.delete(file);
    setDrafts((prev) => new Map(prev).set(file, { content: prev.get(file)?.content ?? item.contents[file] ?? "", ...prev.get(file), ...change }));
  }

  function save() {
    if (edits.length === 0 || oversized) return;
    const submitted = new Map([...drafts].filter(([file]) => dirty.has(file)));
    for (const file of submitted.keys()) submittedFiles.current.set(file, submitted);
    setPendingSaves((current) => new Set(current).add(submitted));
    setFailedSaves((current) => new Set([...current].filter((file) => !submitted.has(file))));
    setSaveError(null);
    writer.commit(saveConfigCommand(state.environment, state.config.name, edits), SHOWN_REFUSALS).isPersisted.promise.then(() => {
      setDrafts((current) => new Map([...current].filter(([file, value]) => submitted.get(file) !== value)));
    }).catch((error) => {
      setFailedSaves((current) => new Set([...current, ...[...submitted.keys()].filter((file) => submittedFiles.current.get(file) === submitted)]));
      setDrafts((current) => new Map([
        ...[...submitted].filter(([file]) => submittedFiles.current.get(file) === submitted && !current.has(file)),
        ...current,
      ]));
      if ([...submitted.keys()].some((file) => submittedFiles.current.get(file) === submitted)) {
        setSaveError(error instanceof StoreRefused ? error.message : "Couldn't save. Try again.");
      }
    }).finally(() => {
      setPendingSaves((current) => new Set([...current].filter((pending) => pending !== submitted)));
    });
  }

  function removeFile(file: string) {
    setFailedSaves((current) => new Set([...current].filter((name) => name !== file)));
    const submitted = new Map([...drafts].filter(([name]) => name === file));
    submittedFiles.current.set(file, submitted);
    setDrafts((current) => new Map([...current].filter(([name]) => name !== file)));
    if (!stored.includes(file)) return;
    writer.commit({ command: "remove_config_file", environment: state.environment, config: state.config.name, file }, SHOWN_REFUSALS)
      .isPersisted.promise.catch((error) => {
        if (submittedFiles.current.get(file) !== submitted) return;
        setDrafts((current) => new Map([...submitted, ...current]));
        setSaveError(error instanceof StoreRefused ? error.message : "Couldn't remove the file.");
      });
  }

  const references = fileNames.flatMap((file) => configReferences(textOf(file), values, serviceNames));
  const dependsOn = [...new Set(references.flatMap((ref) => ref.kind === "ref" ? [ref.service] : []))];
  const secrets = [...new Set(references.flatMap((ref) => ref.kind === "ref" && ref.secret ? [`${ref.service}.${ref.key}`] : []))];

  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-4 pt-4 pb-8">
      <div className="mx-auto flex w-full max-w-2xl flex-col gap-6">
        <SettingsSection id="files" title="Files">
          <ConfigFiles fileNames={fileNames} current={current} onSelect={setSelected} item={item} drafts={drafts} dirty={dirty}
            textOf={textOf} targets={targets} serviceNames={serviceNames} values={values} readOnly={removing}
            onDraft={draft} onSave={save} onRemove={removeFile}
            onAdd={(file) => { draft(file, { content: "" }); setSelected(file); }}
            footer={removing ? null : (
              <div className="flex items-center justify-end gap-3">
                {saveError ?? oversized?.error ? <FieldError className="mr-auto">{saveError ?? `${oversized?.file}: ${oversized?.error}`}</FieldError> : null}
                {unsaved.size > 0 ? (
                  <Button variant="ghost" onClick={() => { submittedFiles.current.clear(); setPendingSaves(new Set()); setFailedSaves(new Set()); setDrafts(new Map()); setSaveError(null); }}>Discard</Button>
                ) : null}
                <Button onClick={save} disabled={dirty.size === 0 || oversized !== undefined}>Save</Button>
              </div>
            )} />
        </SettingsSection>
        <SettingsSection id="depends-on" title="Depends on">
          {dependsOn.length === 0 ? <p className="text-sm text-muted-foreground">No references.</p> : (
            <div className="flex flex-wrap gap-2">{dependsOn.map((service) => <Badge key={service} variant="outline">{service}</Badge>)}</div>
          )}
        </SettingsSection>
        <SettingsSection id="secrets" title="Secrets">
          {secrets.length === 0 ? <p className="text-sm text-muted-foreground">No secrets.</p> : (
            <div className="flex flex-wrap gap-2">
              {secrets.map((secret) => <Badge key={secret} variant="outline"><LockIcon data-icon="inline-start" />{secret}</Badge>)}
            </div>
          )}
        </SettingsSection>
        <SettingsSection id="mounts" title="Mounts">
          {removing ? (
            <Empty variant="placeholder"><EmptyDescription>Removed on next deploy.</EmptyDescription></Empty>
          ) : <ConfigMounts state={state} services={services} />}
        </SettingsSection>
        <SettingsSection id="size" title="Size">
          <div className="flex flex-col gap-1 text-sm">
            {fileNames.map((file) => {
              const bytes = utf8Bytes(textOf(file));
              const error = configFileSizeError(bytes);
              return (
                <div key={file} className="flex items-center gap-3">
                  <span className="min-w-0 flex-1 truncate font-mono">{file}</span>
                  <span className={cn("tabular-nums", error ? "text-destructive" : "text-muted-foreground")}>
                    {error ?? configBytesText(bytes)}
                  </span>
                </div>
              );
            })}
            <p className="text-muted-foreground">Up to 256 KB per file.</p>
          </div>
        </SettingsSection>
        <SettingsSection id="danger" title="Danger">
          <ConfigDanger state={state} params={params} version={version} />
        </SettingsSection>
      </div>
      <AlertDialog open={blocker.status === "blocked"} onOpenChange={(open) => { if (!open) blocker.reset?.(); }}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Discard unsaved changes?</AlertDialogTitle>
            <AlertDialogDescription>Edits to {[...unsaved].join(", ")} are lost.</AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel onClick={() => blocker.reset?.()}>Keep editing</AlertDialogCancel>
            <AlertDialogAction variant="destructive" onClick={() => blocker.proceed?.()}>Discard</AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  );
}

function ConfigFiles({ fileNames, current, onSelect, item, drafts, dirty, textOf, targets, serviceNames, values, readOnly,
  onDraft, onSave, onRemove, onAdd, footer }: {
  fileNames: readonly string[]; current: string | null; onSelect: (file: string) => void; item: ConfigItemView;
  drafts: ReadonlyMap<string, FileDraft>; dirty: ReadonlySet<string>; textOf: (file: string) => string;
  targets: readonly ReferenceTarget[]; serviceNames: readonly string[]; values: ReturnType<typeof configReferenceValues>;
  readOnly: boolean; onDraft: (file: string, change: Partial<FileDraft>) => void; onSave: () => void; onRemove: (file: string) => void;
  onAdd: (file: string) => void; footer: React.ReactNode;
}) {
  const ConfigFileEditor = useConfigFileEditor();
  const [view, setView] = useState<"edit" | "preview">("edit");
  const [adding, setAdding] = useState<{ name: string; error: string | null } | null>(null);

  function add() {
    if (!adding) return;
    const name = adding.name.trim();
    const error = name === "" ? "Enter a file name."
      : fileNames.includes(name) ? `${name} already exists.` : null;
    if (error) return setAdding({ ...adding, error });
    onAdd(name);
    setAdding(null);
  }

  return (
    <div className="flex flex-col gap-3">
      <div className="flex items-center gap-2">
        <div role="tablist" aria-label="Files" className="flex min-w-0 flex-1 flex-wrap items-center gap-1">
          {fileNames.map((file) => {
            const summary = item.files.find((one) => one.name === file);
            const mode = drafts.get(file)?.mode ?? summary?.mode ?? READ_ONLY_MODE;
            const access = fileAccess({ mode, uid: summary?.uid ?? 0, gid: summary?.gid ?? 0 });
            const active = file === current;
            return (
              <div key={file} className={cn("flex items-center rounded-md border", active ? "bg-muted" : "border-transparent")}>
                <button type="button" role="tab" aria-selected={active} onClick={() => onSelect(file)}
                  className="flex items-center gap-1.5 py-1 pl-2.5 pr-1 font-mono text-xs">
                  {file}
                  {mode === EXECUTABLE_MODE ? <span className="text-muted-foreground" title="Executable">x</span> : null}
                  {dirty.has(file) ? <span className="size-1.5 rounded-full bg-foreground" aria-label="Unsaved" /> : null}
                </button>
                {readOnly ? null : (
                  <DropdownMenu>
                    <DropdownMenuTrigger render={<Button variant="ghost" size="icon-xs" aria-label={`${file} options`} />}>
                      <EllipsisIcon />
                    </DropdownMenuTrigger>
                    <DropdownMenuContent align="start">
                      <DropdownMenuGroup>
                        {access.kind === "toggle" ? (
                          <DropdownMenuCheckboxItem checked={access.executable}
                            onCheckedChange={(executable) => onDraft(file, { mode: executable ? EXECUTABLE_MODE : READ_ONLY_MODE })}>
                            Executable
                          </DropdownMenuCheckboxItem>
                        ) : (
                          <>
                            <DropdownMenuCheckboxItem checked={false} disabled>Executable</DropdownMenuCheckboxItem>
                            <DropdownMenuLabel className="font-mono">{access.label}</DropdownMenuLabel>
                          </>
                        )}
                        <DropdownMenuItem variant="destructive" onClick={() => onRemove(file)}>Remove file</DropdownMenuItem>
                      </DropdownMenuGroup>
                    </DropdownMenuContent>
                  </DropdownMenu>
                )}
              </div>
            );
          })}
          {readOnly ? null : (
            <Button variant="ghost" size="icon-sm" aria-label="Add file" onClick={() => setAdding({ name: "", error: null })}>
              <PlusIcon />
            </Button>
          )}
        </div>
        {current === null ? null : (
          <ToggleGroup variant="outline" size="sm" spacing={0} value={[view]}
            onValueChange={([next]) => { if (next === "edit" || next === "preview") setView(next); }}>
            <ToggleGroupItem value="edit">Edit</ToggleGroupItem>
            <ToggleGroupItem value="preview">Preview</ToggleGroupItem>
          </ToggleGroup>
        )}
      </div>
      {adding ? (
        <Field data-invalid={adding.error ? true : undefined}>
          <FieldLabel htmlFor="config-new-file">File name</FieldLabel>
          <div className="flex gap-2">
            <Input id="config-new-file" className="font-mono" value={adding.name} placeholder="config.yml" autoFocus autoComplete="off"
              aria-invalid={adding.error ? true : undefined}
              onChange={(event) => setAdding({ name: event.target.value, error: null })}
              onKeyDown={(event) => { if (event.key === "Enter") add(); if (event.key === "Escape") setAdding(null); }} />
            <Button variant="outline" onClick={() => setAdding(null)}>Cancel</Button>
            <Button onClick={add}>Add</Button>
          </div>
          {adding.error ? <FieldError>{adding.error}</FieldError> : null}
        </Field>
      ) : null}
      {current === null ? (
        <Empty variant="placeholder"><EmptyDescription>No files.</EmptyDescription></Empty>
      ) : view === "preview" ? (
        <ConfigPreview text={textOf(current)} values={values} />
      ) : ConfigFileEditor === null ? (
        <Skeleton className="h-80 w-full" />
      ) : (
        <div className="overflow-hidden rounded-md border">
          <ConfigFileEditor key={current} fileName={current} value={textOf(current)} targets={targets} values={values} services={serviceNames}
            ariaLabel={`${current} contents`} onSave={onSave}
            onChange={(content) => { if (!readOnly) onDraft(current, { content }); }} />
        </div>
      )}
      {footer}
    </div>
  );
}

function ConfigPreview({ text, values }: { text: string; values: ReturnType<typeof configReferenceValues> }) {
  return (
    <pre aria-label="Preview" className="ph-no-capture max-h-[32rem] overflow-auto rounded-md border bg-muted/40 p-3 font-mono text-xs leading-5 whitespace-pre-wrap">
      {previewSegments(text, values).map((segment, index) =>
        segment.kind === "text" ? segment.text
        : segment.kind === "value" ? <span key={index} className="rounded bg-primary/10 px-0.5 text-foreground">{segment.text}</span>
        : segment.kind === "secret" ? (
          <span key={index} className="inline-flex items-center gap-1 rounded bg-muted px-1 align-middle text-muted-foreground"
            aria-label="Secret"><LockIcon className="size-3" />••••••</span>
        ) : <span key={index} className="text-destructive underline decoration-wavy">{segment.text}</span>)}
    </pre>
  );
}

function ConfigMounts({ state, services }: { state: StoreConfig; services: readonly ServiceListing[] }) {
  const writer = useStoreWriter(state.organizationSlug);
  const defaultDir = `/etc/${state.config.name}`;
  const [adding, setAdding] = useState<{ service: string; dir: string; error: string | null }>({ service: "", dir: defaultDir, error: null });
  const [detachError, setDetachError] = useState<string | null>(null);
  const mounted = new Set(state.config.mounts.map((mount) => mount.service));
  const unmountedOrPicked = (service: ServiceListing) => service.name === adding.service || !mounted.has(service.name);
  const available = services.filter((service) => service.change !== "delete" && unmountedOrPicked(service));

  function attach() {
    if (adding.service === "") return setAdding({ ...adding, error: "Select a service." });
    if (!adding.dir.startsWith("/")) return setAdding({ ...adding, error: "Enter an absolute directory, like /etc/app." });
    const cleared = { service: "", dir: defaultDir, error: null };
    writer.commit(attachConfigCommand(state.environment, adding.service, state.config.name, adding.dir), SHOWN_REFUSALS)
      .isPersisted.promise.catch((error) => {
        setAdding((current) => current.service === "" && current.dir === defaultDir && current.error === null
          ? { ...adding, error: error instanceof StoreRefused ? error.message : "Couldn't mount it." } : current);
      });
    setAdding(cleared);
  }

  function detach(service: string) {
    setDetachError(null);
    writer.commit({ command: "detach_config", environment: state.environment, service, config: state.config.name }, SHOWN_REFUSALS)
      .isPersisted.promise.catch((error: Error) => setDetachError(error instanceof StoreRefused ? error.message : "Couldn't unmount it."));
  }

  return (
    <div className="flex flex-col gap-6">
      {state.config.mounts.length === 0 ? (
        <Empty variant="placeholder"><EmptyDescription>Not mounted.</EmptyDescription></Empty>
      ) : (
        <div className="flex flex-col gap-2">
          {state.config.mounts.map((mount) => (
            <Item key={mount.service} variant="outline">
              <ItemContent>
                <ItemTitle>{mount.service}</ItemTitle>
                <ItemDescription className="truncate font-mono">{mount.dir}</ItemDescription>
              </ItemContent>
              <ItemActions>
                <Button variant="ghost" size="icon-sm" aria-label={`Unmount from ${mount.service}`} onClick={() => detach(mount.service)}>
                  <Trash2Icon />
                </Button>
              </ItemActions>
            </Item>
          ))}
          {detachError ? <FieldError>{detachError}</FieldError> : null}
        </div>
      )}
      {available.length === 0 ? (
        <p className="text-sm text-muted-foreground">{services.length > 0 ? "Every service mounts it." : "Add a service to mount this config."}</p>
      ) : (
        <Field data-invalid={adding.error ? true : undefined}>
          <FieldLabel>Mount on a service</FieldLabel>
          <div className="flex flex-col gap-2 sm:flex-row">
            <Select value={adding.service} onValueChange={(service) => setAdding({ ...adding, service: service ?? "", error: null })}>
              <SelectTrigger className="w-full sm:w-auto sm:flex-1" aria-label="Service"><SelectValue placeholder="Select a service" /></SelectTrigger>
              <SelectContent>
                <SelectGroup>
                  {available.map((service) => <SelectItem key={service.id} value={service.name} label={service.name}>{service.name}</SelectItem>)}
                </SelectGroup>
              </SelectContent>
            </Select>
            <Input className="flex-1 font-mono" aria-label="Directory" value={adding.dir} placeholder={defaultDir}
              aria-invalid={adding.error ? true : undefined}
              onChange={(event) => setAdding({ ...adding, dir: event.target.value, error: null })} />
            <Button onClick={attach}><PlusIcon data-icon="inline-start" />Mount</Button>
          </div>
          {adding.error ? <FieldError>{adding.error}</FieldError> : null}
        </Field>
      )}
    </div>
  );
}

function ConfigDanger({ state, params, version }: { state: StoreConfig; params: ConfigRouteParams; version: string }) {
  const writer = useStoreWriter(state.organizationSlug);
  const navigate = useNavigate();
  const actions = useStoreChangeActions(state.organizationSlug, state.environment, version,
    (deploymentId) => void navigate({ to: DEPLOYMENT_PAGE_ROUTE_TO, params: { ...params, deploymentId } }));
  const { config } = state;
  const removing = config.change === "delete";
  const mounts = config.mounts.length;

  function remove() {
    writer.commit({ command: "delete_config", environment: state.environment, config: config.name });
    void navigate({ to: ENVIRONMENT_INDEX_ROUTE_TO, params, search: (prev) => prev });
  }

  return (
    <>
      <DangerRow
        title={removing ? "Removed on next deploy" : "Delete this config"}
        description={removing ? "Keep the config to undo this."
          : mounts > 0 ? `Unmounts from ${mounts} service${mounts === 1 ? "" : "s"} on next deploy.` : "Removed on next deploy."}
        action={removing ? (
          <Button variant="outline" onClick={() => actions.discard(`configs.${config.name}`)}>Keep config</Button>
        ) : (
          <Button variant="destructive" onClick={remove}>
            <Trash2Icon data-icon="inline-start" />
            Delete config
          </Button>
        )} />
      {actions.dialog}
    </>
  );
}
