import { useState } from "react";
import { plural } from "#/lib/plural";
import { Link, useLoaderData, useParams } from "@tanstack/react-router";
import type { Change, EnvironmentRef } from "@ployz/sdk";
import { BoxIcon, LayersIcon, Maximize2Icon, PlusIcon } from "lucide-react";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Field, FieldContent, FieldDescription, FieldGroup, FieldLabel, FieldSet, FieldLegend } from "#/components/ui/field";
import { Item, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemTitle } from "#/components/ui/item";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "#/components/ui/tabs";
import { ToggleGroup, ToggleGroupItem } from "#/components/ui/toggle-group";
import { diffQuery, requireView, useStoreViews, volumesQuery } from "#/modules/config-store/store-view.queries";
import { serviceSettingRows, settingText } from "#/modules/config-store/store-services";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { saveInstall, saveTemplateState, useInstalls, type TemplateState } from "#/modules/config-store/template-values";
import {
  advance, inputOf, nextServer, place, planInstall, removeServer, runSteps, shownOptions, templateById,
  type InstallPlan, type InstallRecord, type InstallView, type RunStep, type TemplateMember, type TemplateOption,
} from "#/modules/config-store/templates";
import { CanvasInspectorHeader } from "./CanvasInspectorHeader";
import { CanvasInspectorError } from "./CanvasInspectorRouteStates";
import { fieldName, shown, TemplateUpdateDialog } from "./TemplateUpdateDialog";
import { useTemplateView } from "./canvas/template-view";
import { createMembers } from "./canvas/useServiceCreator";
import { StoreNetworkingSection } from "../services/$serviceId/-components/StoreNetworkingSection";
import { storeService, StoreSettingField } from "../services/$serviceId/-components/StoreServiceDrawer";
import { originText } from "../services/$serviceId/-components/StoreTemplateSection";
import { ENVIRONMENT_INDEX_ROUTE_TO, ENVIRONMENT_ROUTE_FROM } from "./environment-route-paths";

type Writer = ReturnType<typeof useStoreWriter>;

/**
 * Stages `plan` in the Working State like any edit and keeps its record. A change that adds or removes members of a
 * spread Install becomes a run, placed and stepped when the user runs it.
 */
function applyPlan(writer: Writer, organization: string, environment: EnvironmentRef, current: InstallRecord, plan: InstallPlan) {
  const changes: Change[] = plan.changes.map((row) => row.target === null
    ? { op: "unset", path: row.path }
    : { op: "set", path: row.path, value: row.target });
  if (changes.length > 0) writer.edit({ environment, changes });
  createMembers(writer, environment, plan.record.install, plan.added);
  // Their volumes stay.
  for (const service of plan.removed) writer.commit({ command: "remove_service", environment, service: service.name });
  const topology = (plan.added.length > 0 || plan.removed.length > 0)
    && (current.install.distinctMachines || plan.record.install.distinctMachines);
  const run = topology ? { from: current.receipt.members.map((member) => member.name), steps: null } : plan.record.run;
  saveInstall(organization, environment, { ...plan.record, run });
}

const tag = (image: string) => image.slice(image.lastIndexOf("/") + 1);
const finished = (steps: readonly RunStep[]) => steps.every((step) => step.state === "done");

export function StoreTemplatePanel({ id }: { id: string }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { store } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const writer = useStoreWriter(params.organizationSlug);
  const { setOpen } = useTemplateView();
  const [updating, setUpdating] = useState(false);
  const [diffResult, volumesResult] = useStoreViews(params.organizationSlug, [diffQuery(store), volumesQuery(store)] as const);
  const installed = useInstalls(params.organizationSlug, store);
  const { services, settings, parent } = installed;
  const diff = requireView(diffResult);
  const instance = installed.installs.find((candidate) => candidate.install.name === id);
  if (!instance) return <CanvasInspectorError noun="Template" />;

  const { template, record, install, release, latest } = instance;
  const save = (next: InstallRecord) => saveInstall(params.organizationSlug, store, next);
  const apply = (plan: InstallPlan) => applyPlan(writer, params.organizationSlug, store, record, plan);
  const running = record.run?.steps != null && !finished(record.run.steps);
  const setInput = (key: string, value: string) =>
    apply(planInstall(instance, release.version, { ...install.inputs, [key]: { state: "value", value } }, services, settings));

  const option = (option: TemplateOption) => {
    if (option.kind === "choice") {
      const input = inputOf(option, install.inputs);
      if (input.state === "conflict") {
        return (
          <Field key={option.key} orientation="responsive">
            <FieldContent><FieldLabel>{option.label}<Badge variant="warning">Conflict</Badge></FieldLabel></FieldContent>
            <div className="flex gap-1.5">
              <Button type="button" size="sm" variant="outline" disabled={running} onClick={() => setInput(option.key, input.here)}>
                {option.choices[input.here] ?? input.here}
              </Button>
              <Button type="button" size="sm" variant="outline" disabled={running} onClick={() => setInput(option.key, input.theirs)}>
                {option.choices[input.theirs] ?? input.theirs} · {parent ?? "Parent"}
              </Button>
            </div>
          </Field>
        );
      }
      return (
        <Field key={option.key} orientation="responsive">
          <FieldContent>
            <FieldLabel>{option.label}{input.state === "unset" && <Badge variant="warning">Not set</Badge>}</FieldLabel>
          </FieldContent>
          <ToggleGroup variant="outline" size="sm" spacing={0} aria-label={option.label} disabled={running}
            value={input.state === "value" ? [input.value] : []}
            onValueChange={([picked]) => { if (picked) setInput(option.key, picked); }}>
            {Object.entries(option.choices).map(([value, label]) => <ToggleGroupItem key={value} value={value}>{label}</ToggleGroupItem>)}
          </ToggleGroup>
        </Field>
      );
    }
    // The Service's own control, as its drawer shows it.
    const member = services.find((service) => service.name === option.service);
    if (!member) return null;
    const state = storeService(params.organizationSlug, store, member, settings, diff, writer);
    const label = <>{option.label}<Badge variant="outline"><BoxIcon data-icon="inline-start" />{option.service}</Badge></>;
    return option.setting === "domains"
      ? <StoreNetworkingSection key={option.key} state={state} version={diff.version} validatePrivateDns={() => null} portal={{ label }} />
      : <StoreSettingField key={option.key} state={state} name={option.setting} label={label} />;
  };

  const undecided = Object.entries(record.decisions).flatMap(([path, decision]) => decision === null ? [path] : []);
  const decide = (path: string, decision: "keep" | "remove") => {
    if (decision === "remove") writer.edit({ environment: store, changes: [{ op: "unset", path }] });
    save({ ...record, decisions: { ...record.decisions, [path]: decision } });
  };

  return (
    <div className="flex h-full min-h-0 flex-col">
      <CanvasInspectorHeader params={params}
        actions={latest && <Button type="button" size="sm" variant="outline" disabled={running} onClick={() => setUpdating(true)}>Update to v{latest.version}</Button>}>
        <span className="flex items-center gap-2 font-medium">
          <LayersIcon className="size-4" />{install.name}
          {instance.toFix > 0 && <Badge variant="warning">{instance.toFix} to fix</Badge>}
        </span>
        <p className="truncate text-sm text-muted-foreground">{template.name} v{release.version} · {plural(instance.members.length, "service")}</p>
      </CanvasInspectorHeader>
      <Tabs defaultValue="settings" className="min-h-0 flex-1 gap-0">
        <div className="border-b px-4 pt-1">
          <TabsList variant="line">
            <TabsTrigger value="settings">Settings</TabsTrigger>
            <TabsTrigger value="services">Services</TabsTrigger>
          </TabsList>
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto p-4">
          <TabsContent value="settings" className="flex flex-col gap-6">
            {undecided.length > 0 && (
              <FieldSet>
                <FieldLegend>To fix</FieldLegend>
                {undecided.map((path) => <Undecided key={path} path={path} instance={instance} settings={installed.settings} onDecide={decide} />)}
              </FieldSet>
            )}
            <FieldGroup>{shownOptions(release, install.inputs).map(option)}</FieldGroup>
            {(record.run || install.distinctMachines) && (
              <Run record={record} servers={installed.state?.servers ?? []}
                onSave={save}
                onServers={(change) => saveTemplateState(params.organizationSlug, store, change)} />
            )}
            {instance.edited.length > 0 && (
              <FieldSet>
                <FieldLegend>Edits</FieldLegend>
                {instance.edited.map((row) => (
                  <Field key={row.path} orientation="horizontal">
                    <FieldContent>
                      <FieldLabel>{row.service} · {fieldName(row)}<Badge variant="outline">{originText(row, parent)}</Badge></FieldLabel>
                      <FieldDescription className="truncate font-mono">{shown(row, row.base)} → {shown(row, row.value)}</FieldDescription>
                    </FieldContent>
                  </Field>
                ))}
              </FieldSet>
            )}
          </TabsContent>
          <TabsContent value="services" className="flex flex-col gap-3">
            <Members instance={instance} />
            <Button type="button" variant="outline" className="self-start" onClick={() => setOpen(install.name)}
              render={<Link to={ENVIRONMENT_INDEX_ROUTE_TO} params={params} search={{}} />} nativeButton={false}>
              <Maximize2Icon data-icon="inline-start" />
              Expand
            </Button>
          </TabsContent>
        </div>
      </Tabs>
      {updating && latest && (
        <TemplateUpdateDialog instance={instance} services={services} settings={settings} volumes={requireView(volumesResult).volumes}
          onClose={() => setUpdating(false)}
          onConfirm={(plan, decisions) => {
            for (const [path, decision] of Object.entries(decisions)) {
              if (decision === "remove") writer.edit({ environment: store, changes: [{ op: "unset", path }] });
            }
            apply({ ...plan, record: { ...plan.record, decisions: { ...plan.record.decisions, ...decisions } } });
            setUpdating(false);
          }} />
      )}
    </div>
  );
}

/** An edited row the release dropped: kept as edited, or removed with it. */
function Undecided({ path, instance, settings, onDecide }: {
  path: string;
  instance: InstallView;
  settings: Parameters<typeof serviceSettingRows>[0];
  onDecide: (path: string, decision: "keep" | "remove") => void;
}) {
  const dot = path.indexOf(".");
  const service = path.slice(0, dot);
  const field = path.slice(dot + 1);
  const value = settingText(serviceSettingRows(settings, service).get(field)?.value);
  return (
    <Field orientation="responsive">
      <FieldContent>
        <FieldLabel>{service} · {fieldName({ field })}</FieldLabel>
        <FieldDescription className="truncate">v{instance.release.version} drops it · edited to <span className="font-mono">{shown({ field }, value)}</span></FieldDescription>
      </FieldContent>
      <ToggleGroup variant="outline" size="sm" spacing={0} aria-label={`${service} ${fieldName({ field })}`} value={[]}
        onValueChange={([picked]) => { if (picked === "keep" || picked === "remove") onDecide(path, picked); }}>
        <ToggleGroupItem value="keep">Keep edit</ToggleGroupItem>
        <ToggleGroupItem value="remove">Remove</ToggleGroupItem>
      </ToggleGroup>
    </Field>
  );
}

const stepText = (step: RunStep) =>
  typeof step.state === "object" ? `Waiting on ${step.state.waiting}`
    : step.state === "queued" ? "Queued" : step.state === "running" ? "Running" : "Done";

/**
 * Where the Install's members go and the steps that move them. Placement is matched before anything changes, so a
 * spread that does not fit is refused up front.
 */
function Run({ record, servers, onSave, onServers }: {
  record: InstallRecord;
  servers: TemplateState["servers"];
  onSave: (record: InstallRecord) => void;
  onServers: (change: (state: TemplateState) => TemplateState) => void;
}) {
  const names = record.receipt.members.map((member) => member.name);
  const placement = place(names, servers, record.placement, record.install.distinctMachines);
  const steps = record.run?.steps ?? null;
  const start = () => {
    if (!placement.ok || !record.run) return;
    const placed = { ...record.placement, ...placement.placement };
    onSave({ ...record, placement: placed,
      run: { ...record.run, steps: advance(runSteps(record.run.from, names, record.install.name, placed), servers) } });
  };
  const step = (next: RunStep[]) => record.run && onSave({ ...record, run: { ...record.run, steps: next } });
  const forget = (server: string) => {
    onServers((state) => ({ ...state, servers: state.servers.filter((candidate) => candidate.name !== server) }));
    if (steps) step(removeServer(steps, server));
  };

  return (
    <FieldSet>
      <FieldLegend>Servers</FieldLegend>
      <div className="flex flex-wrap items-center gap-1.5">
        <Badge variant="outline">Mock</Badge>
        {servers.map((server) => (
          <Button key={server.name} type="button" size="xs" variant={server.online ? "outline" : "ghost"}
            aria-pressed={server.online}
            onClick={() => onServers((state) => ({ ...state,
              servers: state.servers.map((candidate) => candidate.name === server.name ? { ...candidate, online: !candidate.online } : candidate) }))}>
            <span className={server.online ? "size-1.5 rounded-full bg-success" : "size-1.5 rounded-full bg-muted-foreground"} />
            {server.name}{!server.online && " · offline"}
          </Button>
        ))}
        <Button type="button" size="xs" variant="ghost"
          onClick={() => onServers((state) => ({ ...state, servers: [...state.servers, nextServer(state.servers)] }))}>
          <PlusIcon data-icon="inline-start" />Server
        </Button>
      </div>
      {record.run && steps === null && (placement.ok
        ? <Button type="button" size="sm" className="self-start" onClick={start}>Run</Button>
        : <p className="text-sm text-destructive">Needs {placement.needs} servers · {placement.available} available</p>)}
      {steps && (
        <ItemGroup className="gap-1.5">
          {steps.map((current, index) => (
            <Item key={index} size="xs" variant="outline">
              <ItemContent className="min-w-0">
                <ItemTitle>
                  {current.label}
                  {current.kind === "job" && <Badge variant="outline">Job</Badge>}
                </ItemTitle>
                <ItemDescription>
                  {[current.server, current.kind === "job" ? "Runs beside deploys" : null].filter(Boolean).join(" · ") || "All servers"}
                </ItemDescription>
              </ItemContent>
              <ItemActions>
                <Badge variant={current.state === "done" ? "outline" : typeof current.state === "object" ? "warning" : "changed"}>{stepText(current)}</Badge>
                {typeof current.state === "object" && (
                  <Button type="button" size="xs" variant="outline" onClick={() => forget(current.server ?? "")}>Remove server</Button>
                )}
              </ItemActions>
            </Item>
          ))}
          {!finished(steps) && (
            <Button type="button" size="sm" variant="outline" className="self-start" onClick={() => step(advance(steps, servers))}>
              Next · mock
            </Button>
          )}
        </ItemGroup>
      )}
    </FieldSet>
  );
}

/** Its Services with where each runs, a nested template's under that template. */
function Members({ instance }: { instance: InstallView }) {
  const row = (member: TemplateMember) => (
    <Item key={member.name} size="xs" variant="outline">
      <ItemContent className="min-w-0">
        <ItemTitle>{member.name}</ItemTitle>
        <ItemDescription className="truncate font-mono">{tag(member.image)}</ItemDescription>
      </ItemContent>
      {instance.record.placement[member.name] && <ItemActions><Badge variant="outline">{instance.record.placement[member.name]}</Badge></ItemActions>}
    </Item>
  );
  return (
    <ItemGroup className="gap-1.5">
      {instance.rendered.filter((member) => !member.via).map(row)}
      {instance.release.uses.map((use) => {
        const nested = instance.rendered.filter((member) => member.via === use.template);
        if (nested.length === 0) return null;
        return (
          <div key={use.as} className="flex flex-col gap-1.5 rounded-lg border p-2">
            <span className="flex items-center gap-2 px-1 text-sm font-medium">
              <LayersIcon className="size-4 text-muted-foreground" />{templateById(use.template)?.name} v{use.version}
            </span>
            {nested.map(row)}
          </div>
        );
      })}
    </ItemGroup>
  );
}
