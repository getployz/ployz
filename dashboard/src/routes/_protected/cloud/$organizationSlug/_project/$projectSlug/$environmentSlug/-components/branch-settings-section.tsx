import { useNavigate } from "@tanstack/react-router";
import { Button } from "#/components/ui/button";
import { FieldDescription, FieldLegend, FieldSet } from "#/components/ui/field";
import { Item, ItemContent, ItemMedia, ItemTitle } from "#/components/ui/item";
import { Switch } from "#/components/ui/switch";
import type { BranchRow } from "#/collections/collections";
import { useKeepBranch } from "#/modules/branches/branch.collection";
import { useWorkspace } from "#/modules/environment-design/workspace.queries";
import { canShutDown } from "#/modules/pr-environments/off";
import { usePrEnvironmentOff } from "#/modules/pr-environments/off-commands";
import { descendants } from "#/modules/project/environment-tree";
import { defaultEnvironmentRefusal } from "#/modules/runtime/teardown";
import { useDeletionNodes, useEnvironmentPlace } from "#/routes/_protected/cloud/$organizationSlug/-components/deletion-items";
import { TeardownDangerSection } from "#/routes/_protected/cloud/$organizationSlug/-components/teardown-danger-section";
import { CloseBranchRow } from "./close-branch-row";

/**
 * What a Branch's Manage panel does to the Branch itself: Keep it, shut a PR Environment down, and close it. A Branch
 * that isn't kept and has none of its own closes with one plain confirm; the rest go through Danger, typed.
 */
export function BranchSections({ organizationSlug, projectSlug, environmentSlug, branch, name, parent }: {
  organizationSlug: string;
  projectSlug: string;
  environmentSlug: string;
  branch: BranchRow;
  name: string;
  parent: { name: string; namespace: string };
}) {
  const navigate = useNavigate();
  const keepBranch = useKeepBranch(organizationSlug);
  const { projects, environments, branches } = useWorkspace(organizationSlug);
  const { shutDown } = usePrEnvironmentOff({ organizationSlug, environmentId: branch.environmentId, name });
  const own = useDeletionNodes(organizationSlug, { projectSlug, environmentSlug });
  const place = useEnvironmentPlace(organizationSlug, branch.environmentId);
  const project = projects.find((row) => row.slug === projectSlug);
  // A teardown takes the Branch's own Branches with it, deepest first.
  const closing = descendants(branch.environmentId, branches).flatMap((id) => environments.filter((row) => row.id === id));
  const defaultEnvironment = closing.find((row) => row.id === project?.defaultEnvironmentId);
  const closesHere = !branch.kept && closing.length === 0;
  const { pullRequest } = branch;
  return (
    <>
      {!pullRequest && (
        <FieldSet>
          <FieldLegend>Keep</FieldLegend>
          <FieldDescription>Otherwise it can be deleted after saving, and closes after 7 days without a deploy.</FieldDescription>
          <Item variant="muted" render={<label htmlFor="keep-branch" />}>
            <ItemMedia><Switch id="keep-branch" checked={branch.kept} onCheckedChange={(kept) => keepBranch(branch.environmentId, kept)} /></ItemMedia>
            <ItemContent><ItemTitle>Keep this branch</ItemTitle></ItemContent>
          </Item>
        </FieldSet>
      )}
      {pullRequest && !pullRequest.closed && canShutDown(pullRequest.shutdown) && (
        <FieldSet className="items-start">
          <FieldLegend>Shut down</FieldLegend>
          <FieldDescription>Stops its services and keeps its settings. It starts again on the next push.</FieldDescription>
          <Button variant="outline" disabled={shutDown.isPending} onClick={() => shutDown.mutate()}>Shut down {name}</Button>
        </FieldSet>
      )}
      {closesHere ? (
        <FieldSet>
          <FieldLegend>Close</FieldLegend>
          <CloseBranchRow organizationSlug={organizationSlug} projectSlug={projectSlug} environmentId={branch.environmentId} name={name}
            parentNamespace={parent.namespace} reopensWith={pullRequest && !pullRequest.closed ? pullRequest.number : null}
            own={own.map((item) => item.name)} />
        </FieldSet>
      ) : (
        <TeardownDangerSection
          organizationSlug={organizationSlug}
          scope="environment"
          environmentId={branch.environmentId}
          name={name}
          place={place}
          verb="Close"
          title="Close this branch"
          description="Its services and data go with it."
          actionLabel="Close branch"
          items={[...own, ...closing.map((row) => ({ kind: "branch" as const, name: row.name }))]}
          disabledReason={defaultEnvironment && defaultEnvironmentRefusal(defaultEnvironment.name)}
          headingId="branch-teardown-heading"
          onCompleted={() => void navigate({ to: "/cloud/$organizationSlug/$projectSlug/$environmentSlug",
            params: { organizationSlug, projectSlug, environmentSlug: parent.namespace }, replace: true })}
        />
      )}
    </>
  );
}
