import { useState } from "react";
import { useLoaderData, useParams } from "@tanstack/react-router";
import { Button } from "#/components/ui/button";
import { FieldDescription, FieldError, FieldGroup, FieldLegend, FieldSet } from "#/components/ui/field";
import { Item, ItemContent, ItemMedia, ItemTitle } from "#/components/ui/item";
import { Spinner } from "#/components/ui/spinner";
import { Switch } from "#/components/ui/switch";
import { useClusterDomainName } from "#/modules/cluster-domain/use-cluster-domain";
import { useDeploymentAttempt } from "#/modules/deployments/deployment.collection";
import { useEnvironmentDocument } from "#/modules/environment-design/environment-document.collection";
import { useWorkspace } from "#/modules/environment-design/workspace.queries";
import { useCreateBranch } from "#/modules/branches/branch-commands";
import { useLineageNames } from "#/modules/branches/use-lineage-names";
import { branchHostnameSuffix, branchNameError, branchSetupCommands, branchNamespace, defaultBranchName, ownLineages } from "#/modules/branches/branch-plan";
import { CanvasInspectorHeader } from "../CanvasInspectorHeader";
import { ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";
import { useBranchPicking } from "./branch-picking";
import { NameSection } from "./NameSection";
import { ServicesSection } from "./ServicesSection";
import { SetupSection } from "./SetupSection";

/**
 * "New branch of X": name it, pick what gets an Own Copy, create and deploy it, or just create it as a starting point. The picks
 * live in `BranchPickingProvider`, shared with the canvas under the panel. `focus` is the lineage it opened on. With `fix`,
 * a failed attempt, the focused service carries the change that failed while it keeps its own copy (Fix it on a branch).
 */
export function NewBranchPanel({ focus: initialFocus, fix }: { focus: string | null; fix: string | null }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { environmentId } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const picking = useBranchPicking();
  const document = useEnvironmentDocument(params.organizationSlug, environmentId);
  const { environments, branches } = useWorkspace(params.organizationSlug);
  const lineageName = useLineageNames(params.organizationSlug);
  const clusterDomain = useClusterDomainName(params.organizationSlug);
  const create = useCreateBranch(params.projectSlug);
  const failedAttempt = useDeploymentAttempt(params.organizationSlug, environmentId, fix).attempt?.deployment;
  const taken = new Set(environments.map((environment) => environment.namespace));
  const [name, setName] = useState<string | null>(null);
  const [keep, setKeep] = useState(false);
  const [setupCommands, setSetupCommands] = useState(() => document?.branchSetupCommands ?? []);
  if (!picking) return null;

  const { parent, intent, plan, focus, picks } = picking;
  const own = ownLineages(plan);
  const nameOf = (lineage: string) => lineageName(lineage, environmentId);

  // The failed service, while it keeps its own copy: its node in the attempt, and the Parent's service it failed on.
  const failedService = intent.services.find((node) => node.lineageId === initialFocus);
  const failedNode = failedAttempt?.status === "failed" && failedService && own.includes(failedService.lineageId)
    ? failedAttempt.targetNodes.nodes.find((node) => node.nodeId === failedService.id) : undefined;
  const failedChange = failedNode?.settings?.[0];
  const moreChanges = (failedNode?.settings?.length ?? 1) - 1;

  const branchName = name ?? defaultBranchName(params.projectSlug, failedNode ? `fix-${failedNode.name}` : "new-branch", taken);
  const nameError = branchNameError(params.projectSlug, branchName, taken);
  const fromSuffix = branchHostnameSuffix(params.projectSlug, parent.namespace, branches.some((row) => row.environmentId === parent.id));
  const intoSuffix = branchHostnameSuffix(params.projectSlug, branchNamespace(params.projectSlug, branchName), true);
  // ponytail: mirrors core's suffix swap for the preview only; the server's addresses come from core's branchChanges.
  const addresses = intent.services.filter((node) => own.includes(node.lineageId))
    .flatMap((node) => node.config.managedHostnames.map(({ prefix }) =>
      `${prefix.endsWith(fromSuffix) ? prefix.slice(0, prefix.length - fromSuffix.length) : prefix}${intoSuffix}${clusterDomain ? `.${clusterDomain}` : ""}`));

  const blocked = own.length === 0 ? "Pick something to change" : null;
  const submit = (deployNow: boolean) => {
    if (blocked || nameError || create.isPending) return;
    create.mutate({
      organizationSlug: params.organizationSlug, parentEnvironmentId: parent.id, name: branchName.trim(), focus, picks, keep, deployNow,
      fix: failedNode && fix ? { deploymentId: fix, serviceId: failedNode.nodeId } : undefined,
      // Only a Branch with an Own Copy of data shows Setup command; a command for a service that isn't own is dropped.
      setupCommands: branchSetupCommands(plan, setupCommands),
    });
  };
  return (
    <form className="flex h-full min-h-0 flex-col" onSubmit={(event) => { event.preventDefault(); submit(true); }}>
      <CanvasInspectorHeader params={params}>
        <span className="font-medium">{failedNode ? `Fix ${failedNode.name} on a branch` : "New branch"}</span>
        <p className="text-sm break-words text-muted-foreground">
          From {parent.name}{failedNode ? `, with the change that failed${failedChange
            ? `: ${failedChange.label.toLowerCase()} ${failedChange.newValue}${moreChanges > 0 ? ` and ${moreChanges} more` : ""}` : ""}` : null}
        </p>
      </CanvasInspectorHeader>
      <FieldGroup className="min-h-0 flex-1 gap-8 overflow-y-auto p-4">
        <FieldDescription>A copy of {parent.name} to change things in without touching it.</FieldDescription>
        <NameSection name={branchName} onName={setName} error={nameError} addresses={addresses} />
        <ServicesSection picking={picking} nameOf={nameOf} target={branchName.trim() || "this branch"} who="This branch" />
        <SetupSection plan={plan} nameOf={nameOf} setupCommands={setupCommands} onSetupCommands={setSetupCommands} />
        <FieldSet>
          <FieldLegend>After saving</FieldLegend>
          <FieldDescription>Otherwise it can be deleted once its changes are saved, and closes after 7 days without a deploy.</FieldDescription>
          <Item variant="muted" render={<label htmlFor="branch-keep" />}>
            <ItemMedia><Switch id="branch-keep" checked={keep} onCheckedChange={setKeep} /></ItemMedia>
            <ItemContent><ItemTitle>Keep it after saving</ItemTitle></ItemContent>
          </Item>
        </FieldSet>
      </FieldGroup>
      <div className="flex shrink-0 flex-col gap-2 border-t p-4">
        {create.isError && <FieldError>{create.error.message}</FieldError>}
        {blocked ? <Button type="submit" disabled>{blocked}</Button> : (
          <div className="flex gap-2">
            <Button type="submit" className="flex-1" disabled={Boolean(nameError) || create.isPending}>
              {create.isPending && create.variables?.deployNow && <Spinner data-icon="inline-start" />}Create and deploy
            </Button>
            <Button type="button" variant="outline" className="flex-1" disabled={Boolean(nameError) || create.isPending} onClick={() => submit(false)}>
              {create.isPending && !create.variables?.deployNow && <Spinner data-icon="inline-start" />}Just create
            </Button>
          </div>
        )}
      </div>
    </form>
  );
}
