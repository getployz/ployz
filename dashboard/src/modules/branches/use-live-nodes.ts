import { useLiveQuery } from "@tanstack/react-db";
import { getEnvironmentsCollection, getRawServicesCollection } from "#/collections/collections";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { useEnvironmentChangeStates } from "#/modules/deployments/environment-change-state.queries";
import type { EnvironmentChangeStateNodeProjection } from "#/modules/deployments/deployment-contract";
import { useWorkspace } from "#/modules/environment-design/workspace.queries";
import { liveNodeUsers, liveOwner } from "./live-owner";

type Workspace = ReturnType<typeof useWorkspace>;
export type LiveNodeOwner = { environment: Workspace["environments"][number]; node: EnvironmentChangeStateNodeProjection };

/** Whose a Live Node is, for a Branch of `parentId`: the Environment that runs it and the node as it runs there. */
export function useLiveOwner(organizationSlug: string) {
  const states = useEnvironmentChangeStates(organizationSlug, useCollectionScope());
  const { environments, branches } = useWorkspace(organizationSlug);
  const applied = new Map(states.map((state) => [state.environmentId, new Set(state.applied.nodes.map((node) => node.nodeLineageId))]));
  return (parentId: string, lineageId: string): LiveNodeOwner | null => {
    const ownerId = liveOwner(parentId, lineageId, branches, applied);
    const environment = environments.find((candidate) => candidate.id === ownerId);
    const node = states.find((state) => state.environmentId === ownerId)?.applied.nodes.find((candidate) => candidate.nodeLineageId === lineageId);
    return environment && node ? { environment, node } : null;
  };
}

export type LiveNode = {
  lineageId: string;
  name: string;
  /** Null when no ancestor runs it. */
  owner: LiveNodeOwner | null;
  /** The services here that use it. */
  usedBy: string[];
  /** It mounts a Volume where it runs: the Branch reads and writes that real data. */
  ownsData: boolean;
};

/** A Branch's Live Nodes: what its Own Copies use but don't own. A root Environment has none. */
export function useLiveNodes(organizationSlug: string, environmentId: string): LiveNode[] {
  const scope = useCollectionScope();
  const { branches } = useWorkspace(organizationSlug);
  const ownerOf = useLiveOwner(organizationSlug);
  const { data: environments } = useLiveQuery(getEnvironmentsCollection(organizationSlug, scope));
  const { data: services } = useLiveQuery(getRawServicesCollection(organizationSlug, scope));
  const parentId = branches.find((branch) => branch.environmentId === environmentId)?.parentEnvironmentId;
  const intent = environments.find((environment) => environment.id === environmentId)?.intent;
  if (!parentId || !intent) return [];
  return [...liveNodeUsers(intent)].map(([lineageId, usedBy]) => {
    const owner = ownerOf(parentId, lineageId);
    const authored = environments.find((environment) => environment.id === owner?.environment.id)?.intent.services.find((service) => service.lineageId === lineageId);
    const name = services.find((row) => row.lineageId === lineageId && row.environmentId === owner?.environment.id)?.name
      ?? services.find((row) => row.lineageId === lineageId)?.name ?? authored?.slug ?? "a service";
    return { lineageId, name, owner, usedBy, ownsData: (authored?.volumeAttachments.length ?? 0) > 0 };
  });
}
