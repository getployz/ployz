import { useNavigate } from "@tanstack/react-router";
import { Item, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemTitle } from "#/components/ui/item";
import { Switch } from "#/components/ui/switch";
import type { BranchRow } from "#/collections/collections";
import { useKeepBranch } from "#/modules/branches/branch.collection";
import { useWorkspace } from "#/modules/environment-design/workspace.queries";
import { descendants } from "#/modules/project/environment-tree";
import { defaultEnvironmentRefusal } from "#/modules/runtime/teardown";
import { useDeletionNodes, useEnvironmentPlace } from "#/routes/_protected/cloud/$organizationSlug/-components/deletion-items";
import { TeardownDangerSection } from "#/routes/_protected/cloud/$organizationSlug/-components/teardown-danger-section";
import { ShutdownRow } from "./branch-review/BranchNews";
import { CloseBranchRow } from "./close-branch-row";

/**
 * What a Branch's panel does to the Branch itself, last and quiet, a line each: Keep it, shut a PR Environment down, and
 * close it. A Branch that isn't kept, isn't the Default Environment and has none of its own closes with one plain
 * confirm; the rest confirm typed. While it's about to close itself, its news offers Keep instead.
 */
export function BranchSections({ organizationSlug, projectSlug, environmentSlug, branch, name, parent, closing }: {
  organizationSlug: string;
  projectSlug: string;
  environmentSlug: string;
  branch: BranchRow;
  name: string;
  parent: { name: string; namespace: string };
  /** Its news says it's about to close itself, with Keep. */
  closing: boolean;
}) {
  const navigate = useNavigate();
  const keepBranch = useKeepBranch(organizationSlug);
  const { projects, environments, branches } = useWorkspace(organizationSlug);
  const own = useDeletionNodes(organizationSlug, { projectSlug, environmentSlug });
  const place = useEnvironmentPlace(organizationSlug, branch.environmentId);
  const project = projects.find((row) => row.slug === projectSlug);
  // A teardown takes the Branch's own Branches with it, deepest first.
  const children = descendants(branch.environmentId, branches).flatMap((id) => environments.filter((row) => row.id === id));
  const defaultEnvironment = environments.find((row) => row.id === project?.defaultEnvironmentId
    && (row.id === branch.environmentId || children.includes(row)));
  const closesHere = !branch.kept && children.length === 0 && !defaultEnvironment;
  const { pullRequest } = branch;
  return (
    <ItemGroup className="gap-2">
      {!pullRequest && !closing && (
        <Item size="sm" render={<label htmlFor="keep-branch" />}>
          <ItemContent>
            <ItemTitle>Keep this branch</ItemTitle>
            <ItemDescription>Otherwise it closes after 7 days without a deploy.</ItemDescription>
          </ItemContent>
          <ItemActions><Switch id="keep-branch" checked={branch.kept} onCheckedChange={(kept) => keepBranch(branch.environmentId, kept)} /></ItemActions>
        </Item>
      )}
      {pullRequest && !pullRequest.closed && pullRequest.shutdown === null && (
        <ShutdownRow environmentId={branch.environmentId} name={name} shutdown={null} />
      )}
      {closesHere ? (
        <CloseBranchRow organizationSlug={organizationSlug} projectSlug={projectSlug} environmentId={branch.environmentId} name={name}
          parentNamespace={parent.namespace} reopensWith={pullRequest && !pullRequest.closed ? pullRequest.number : null}
          own={own.map((item) => item.name)} />
      ) : (
        <TeardownDangerSection
          inline
          organizationSlug={organizationSlug}
          scope="environment"
          environmentId={branch.environmentId}
          name={name}
          place={place}
          verb="Close"
          title={`Close ${name}`}
          description="Its services and data go with it."
          actionLabel="Close"
          items={[...own, ...children.map((row) => ({ kind: "branch" as const, name: row.name }))]}
          disabledReason={defaultEnvironment && defaultEnvironmentRefusal(defaultEnvironment.name)}
          headingId="branch-teardown-heading"
          onCompleted={() => void navigate({ to: "/cloud/$organizationSlug/$projectSlug/$environmentSlug",
            params: { organizationSlug, projectSlug, environmentSlug: parent.namespace }, replace: true })}
        />
      )}
    </ItemGroup>
  );
}
