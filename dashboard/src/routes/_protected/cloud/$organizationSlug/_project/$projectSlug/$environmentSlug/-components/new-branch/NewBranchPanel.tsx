import { useState } from "react";
import { useLiveQuery } from "@tanstack/react-db";
import { useLoaderData, useParams } from "@tanstack/react-router";
import { getRawServicesCollection } from "#/collections/collections";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { Button } from "#/components/ui/button";
import { Field, FieldContent, FieldDescription, FieldError, FieldGroup, FieldLabel, FieldLegend, FieldSet } from "#/components/ui/field";
import { Spinner } from "#/components/ui/spinner";
import { Switch } from "#/components/ui/switch";
import { useClusterDomainName } from "#/modules/cluster-domain/use-cluster-domain";
import { useDeploymentAttempt } from "#/modules/deployments/deployment.collection";
import { useEnvironmentChangeStateProjection } from "#/modules/deployments/environment-change-state.queries";
import { useEnvironmentDocument } from "#/modules/environment-design/environment-document.collection";
import { findEnvironment, useWorkspace } from "#/modules/environment-design/workspace.queries";
import { useCreateBranch } from "#/modules/branches/branch-commands";
import {
  branchHostnameSuffix, branchNameError, branchNamespace, defaultBranchName, offeredPresets, ownLineages, planBranchOf,
  type BranchPicks,
} from "#/modules/branches/branch-plan";
import { CanvasInspectorHeader } from "../CanvasInspectorHeader";
import { ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";
import { DataSection } from "./DataSection";
import { NameSection } from "./NameSection";
import { WhatComesAlongSection } from "./WhatComesAlongSection";

/**
 * "New branch of X": pick what gets an Own Copy, name it, create and deploy it, or keep it as a starting point. Core plans every pick over the Parent's
 * Working State, with "deployed" meaning the Parent's Applied lineages. `focus` seeds what changes. With `fix`, a failed
 * attempt, the focused service carries the change that failed while it keeps its own copy (Fix it on a branch).
 */
export function NewBranchPanel({ focus: initialFocus, fix }: { focus: string | null; fix: string | null }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { environmentId } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const scope = useCollectionScope();
  const document = useEnvironmentDocument(params.organizationSlug, environmentId);
  const applied = useEnvironmentChangeStateProjection({ organizationSlug: params.organizationSlug, environmentId })?.applied.nodes ?? [];
  const { projects, environments, branches } = useWorkspace(params.organizationSlug);
  const { data: services } = useLiveQuery(getRawServicesCollection(params.organizationSlug, scope));
  const clusterDomain = useClusterDomainName(params.organizationSlug);
  const create = useCreateBranch(params.projectSlug);
  const failedAttempt = useDeploymentAttempt(params.organizationSlug, environmentId, fix).attempt?.deployment;
  const parent = findEnvironment(projects, environments, params);
  const taken = new Set(environments.map((environment) => environment.namespace));
  const intent = document?.intent;
  const owned = new Set([...(intent?.services.map((node) => node.lineageId) ?? []), ...(intent?.volumes.map((node) => node.resourceLineageId) ?? [])]);

  const [focus, setFocus] = useState(() => initialFocus && owned.has(initialFocus) ? [initialFocus] : []);
  const [picks, setPicks] = useState<BranchPicks>({ preset: "only" });
  const [name, setName] = useState<string | null>(null);
  const [keep, setKeep] = useState(false);
  const [deployNow, setDeployNow] = useState(true);
  const [setupCommands, setSetupCommands] = useState(() => document?.branchSetupCommands ?? []);
  if (!intent || !parent) return null;

  const planned = { parent: intent, deployed: applied.map((node) => node.nodeLineageId), focus };
  const plan = planBranchOf({ ...planned, picks });
  const presets = offeredPresets(planned).map((preset) => ({ preset, plan: planBranchOf({ ...planned, picks: { preset } }) }));
  const own = ownLineages(plan);
  // Services a Branch uses live may belong to an ancestor, so names come from any Environment with that lineage.
  const nameOf = (lineage: string) => services.find((row) => row.lineageId === lineage && row.environmentId === environmentId)?.name
    ?? services.find((row) => row.lineageId === lineage)?.name
    ?? intent.volumes.find((node) => node.resourceLineageId === lineage)?.name ?? "a service";
  const parentOf = (id: string) => environments.find((environment) =>
    environment.id === branches.find((row) => row.environmentId === id)?.parentEnvironmentId);
  let root = parent;
  for (let up = parentOf(root.id); up; up = parentOf(root.id)) root = up;

  // The failed service, while it keeps its own copy: its node in the attempt, and the Parent's service it failed on.
  const failedService = intent.services.find((node) => node.lineageId === initialFocus);
  const failedNode = failedAttempt?.status === "failed" && failedService && own.includes(failedService.lineageId)
    ? failedAttempt.targetNodes.nodes.find((node) => node.nodeId === failedService.id) : undefined;
  const failedChange = failedNode?.settings?.[0];
  const moreChanges = (failedNode?.settings?.length ?? 1) - 1;

  const branchName = name ?? defaultBranchName(params.projectSlug, failedNode ? `fix-${failedNode.name}` : "new-branch", taken);
  const nameError = branchNameError(params.projectSlug, branchName, taken);
  const fromSuffix = branches.some((row) => row.environmentId === parent.id) ? branchHostnameSuffix(params.projectSlug, parent.namespace) : "";
  const intoSuffix = branchHostnameSuffix(params.projectSlug, branchNamespace(params.projectSlug, branchName));
  // ponytail: mirrors core's suffix swap for the preview only; the server's addresses come from core's branchChanges.
  const addresses = intent.services.filter((node) => own.includes(node.lineageId))
    .flatMap((node) => node.config.managedHostnames.map(({ prefix }) =>
      `${prefix.endsWith(fromSuffix) ? prefix.slice(0, prefix.length - fromSuffix.length) : prefix}${intoSuffix}${clusterDomain ? `.${clusterDomain}` : ""}`));

  function toggle(lineage: string) {
    const next = new Set(own);
    const on = !next.has(lineage);
    if (on) next.add(lineage); else next.delete(lineage);
    setFocus(on ? [...focus, lineage] : focus.filter((candidate) => candidate !== lineage));
    setPicks({ own: [...next] });
  }

  const blocked = own.length === 0 ? "Pick something to copy" : null;
  return (
    <form className="flex h-full min-h-0 flex-col" onSubmit={(event) => {
      event.preventDefault();
      if (blocked || nameError || create.isPending) return;
      // Only a Branch with an Own Copy of data shows Then run; a command for a service that isn't own is dropped.
      const ownData = plan.nodes.some((node) => node.role === "own" && node.nodeType === "volume");
      const commands = ownData ? setupCommands.filter((setup) => setup.command.trim() && own.includes(setup.lineageId)
        && intent.services.some((node) => node.lineageId === setup.lineageId)) : [];
      create.mutate({
        organizationSlug: params.organizationSlug, parentEnvironmentId: parent.id, name: branchName.trim(), focus, picks, keep, deployNow,
        fix: failedNode && fix ? { deploymentId: fix, serviceId: failedNode.nodeId } : undefined,
        setupCommands: commands.map((setup) => ({ ...setup, command: setup.command.trim() })),
      });
    }}>
      <CanvasInspectorHeader params={params}>
        <span className="font-medium">{failedNode ? `Fix ${failedNode.name} on a branch` : "New branch"}</span>
        <p className="text-sm break-words text-muted-foreground">
          From {parent.name}{failedNode ? `, with the change that failed${failedChange
            ? `: ${failedChange.label.toLowerCase()} ${failedChange.newValue}${moreChanges > 0 ? ` and ${moreChanges} more` : ""}` : ""}` : null}
        </p>
      </CanvasInspectorHeader>
      <FieldGroup className="min-h-0 flex-1 overflow-y-auto p-4">
        <WhatComesAlongSection parentName={parent.name} plan={plan} presets={presets} nameOf={nameOf} owned={owned}
          onPreset={(preset) => setPicks({ preset })} onToggle={toggle} />
        <DataSection intent={intent} plan={plan} parentName={parent.name} rootName={root.id === parent.id ? null : root.name} nameOf={nameOf}
          setupCommands={setupCommands} onSetupCommands={setSetupCommands} />
        <NameSection name={branchName} onName={setName} error={nameError} addresses={addresses} />
        <FieldSet>
          <FieldLegend>When it's done</FieldLegend>
          <FieldLabel htmlFor="branch-keep">
            <Field orientation="horizontal">
              <FieldContent>
                <span className="font-medium">Keep it after merging</span>
                <FieldDescription>
                  {keep ? `It stays after merging into ${parent.name}, like staging.`
                    : `It merges into ${parent.name} when you're happy, then closes itself. Or after 7 days without a deploy.`}
                </FieldDescription>
              </FieldContent>
              <Switch id="branch-keep" checked={keep} onCheckedChange={setKeep} />
            </Field>
          </FieldLabel>
        </FieldSet>
        <FieldLabel htmlFor="branch-deploy-now">
          <Field orientation="horizontal">
            <FieldContent>
              <span className="font-medium">Deploy it now</span>
              <FieldDescription>
                {deployNow ? "Its copies start in about a minute." : "Nothing runs. It becomes a starting point that other branches copy."}
              </FieldDescription>
            </FieldContent>
            <Switch id="branch-deploy-now" checked={deployNow} onCheckedChange={setDeployNow} />
          </Field>
        </FieldLabel>
      </FieldGroup>
      <div className="flex shrink-0 flex-col gap-2 border-t p-4">
        {create.isError && <FieldError>{create.error.message}</FieldError>}
        <Button type="submit" disabled={Boolean(blocked || nameError) || create.isPending}>
          {create.isPending && <Spinner data-icon="inline-start" />}
          {blocked ?? (deployNow ? "Create and deploy" : "Create without deploying")}
        </Button>
      </div>
    </form>
  );
}
