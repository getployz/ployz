import { useState } from "react";
import { useNavigate } from "@tanstack/react-router";
import { MoreVerticalIcon } from "lucide-react";
import { ConfirmDialog } from "#/components/confirm-dialog";
import { Button } from "#/components/ui/button";
import {
  DropdownMenu, DropdownMenuCheckboxItem, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger,
} from "#/components/ui/dropdown-menu";
import type { BranchRow } from "#/collections/collections";
import { useKeepBranch } from "#/modules/branches/branch.collection";
import { useWorkspace } from "#/modules/environment-design/workspace.queries";
import { canShutDown } from "#/modules/pr-environments/off";
import { usePrEnvironmentOff } from "#/modules/pr-environments/off-commands";
import { descendants } from "#/modules/project/environment-tree";
import { defaultEnvironmentRefusal } from "#/modules/runtime/teardown";
import { useDeletionNodes, useEnvironmentPlace } from "#/routes/_protected/cloud/$organizationSlug/-components/deletion-items";
import { TeardownDangerSection } from "#/routes/_protected/cloud/$organizationSlug/-components/teardown-danger-section";
import { closeDeletes, closeMode, useCloseBranch } from "./close-branch";

/**
 * What a Branch's panel does to the Branch itself, out of the way in its ⋮: Keep it, shut a PR Environment down, and
 * close it (see closeMode). `menu` sits in the panel's header; `dialogs` holds the close confirmations and a typed
 * close's status, in its body.
 */
export function useBranchLifecycle({ organizationSlug, projectSlug, environmentSlug, branch, name, parent }: {
  organizationSlug: string;
  projectSlug: string;
  environmentSlug: string;
  branch: BranchRow;
  name: string;
  parent: { namespace: string };
}) {
  const navigate = useNavigate();
  const keepBranch = useKeepBranch(organizationSlug);
  const { shutDown } = usePrEnvironmentOff({ organizationSlug, environmentId: branch.environmentId, name });
  const { close, closing } = useCloseBranch({ organizationSlug, projectSlug, environmentId: branch.environmentId, name, parentNamespace: parent.namespace });
  const { projects, environments, branches } = useWorkspace(organizationSlug);
  const own = useDeletionNodes(organizationSlug, { projectSlug, environmentSlug });
  const place = useEnvironmentPlace(organizationSlug, branch.environmentId);
  const [asking, setAsking] = useState(false);
  const project = projects.find((row) => row.slug === projectSlug);
  // A teardown takes the Branch's own Branches with it, deepest first.
  const children = descendants(branch.environmentId, branches).flatMap((id) => environments.filter((row) => row.id === id));
  const defaultEnvironment = environments.find((row) => row.id === project?.defaultEnvironmentId
    && (row.id === branch.environmentId || children.includes(row)));
  const pr = branch.pullRequest;
  const prOpen = pr !== null && !pr.closed;
  const mode = closeMode({ kept: branch.kept, hasBranches: children.length > 0, isDefault: defaultEnvironment !== undefined, prOpen });
  const refusal = defaultEnvironment && defaultEnvironmentRefusal(defaultEnvironment.name);

  const menu = (
    <DropdownMenu>
      <DropdownMenuTrigger render={<Button variant="ghost" size="icon" aria-label="Branch actions" title="Branch actions" />}>
        <MoreVerticalIcon />
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-auto">
        {pr ? null : (
          <DropdownMenuCheckboxItem checked={branch.kept} onCheckedChange={(kept) => keepBranch(branch.environmentId, kept)}>
            Keep this branch
          </DropdownMenuCheckboxItem>
        )}
        {prOpen && canShutDown(pr.shutdown) ? <DropdownMenuItem onClick={() => shutDown.mutate()}>Shut down {name}</DropdownMenuItem> : null}
        {/* The next push brings it back, so it closes without asking. */}
        <DropdownMenuItem variant="destructive" disabled={closing || refusal !== undefined} title={refusal}
          onClick={() => mode === "now" ? void close() : setAsking(true)}>
          {mode === "now" ? "Close until the next push" : `Close ${name}…`}
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );

  const dialogs = mode === "typed" ? (
    <TeardownDangerSection
      inline
      open={asking}
      onOpenChange={setAsking}
      organizationSlug={organizationSlug}
      scope="environment"
      environmentId={branch.environmentId}
      name={name}
      place={place}
      verb="Close"
      title="Close this branch"
      description="Its services and data go with it."
      actionLabel="Close branch"
      items={[...own, ...children.map((row) => ({ kind: "branch" as const, name: row.name }))]}
      disabledReason={refusal}
      headingId="branch-teardown-heading"
      onCompleted={() => void navigate({ to: "/cloud/$organizationSlug/$projectSlug/$environmentSlug",
        params: { organizationSlug, projectSlug, environmentSlug: parent.namespace }, replace: true })}
    />
  ) : mode === "confirm" ? (
    <ConfirmDialog open={asking} onOpenChange={setAsking} title={`Close ${name}?`} description={closeDeletes(own.map((item) => item.name))}
      actionLabel="Close branch" variant="destructive" onConfirm={() => { setAsking(false); return close(); }} />
  ) : null;

  return { menu, dialogs };
}
