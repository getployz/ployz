import type { SavedEnvironmentIntent } from "#/modules/environment-design/saved-intent";

type BranchRow = { environmentId: string; parentEnvironmentId: string };

/**
 * Whose a Live Node is: `parentId` when it runs `lineageId`, else its nearest ancestor that does; null when none does.
 * A Branch's Live Nodes belong to `liveOwner(itsParentId, …)`. "Runs" means the lineage is in that Environment's Applied
 * State, so the browser and admission (#1155) agree on the owner.
 */
export function liveOwner(
  parentId: string,
  lineageId: string,
  branches: Iterable<BranchRow>,
  appliedByEnvironment: ReadonlyMap<string, ReadonlySet<string>>,
): string | null {
  const parentOf = new Map([...branches].map((branch) => [branch.environmentId, branch.parentEnvironmentId]));
  const seen = new Set<string>();
  for (let at: string | undefined = parentId; at && !seen.has(at); at = parentOf.get(at)) {
    if (appliedByEnvironment.get(at)?.has(lineageId)) return at;
    seen.add(at);
  }
  return null;
}

/** The lineages this intent's services use but don't own (its Live Nodes), each with the ids of the services using it. */
export function liveNodeUsers(intent: SavedEnvironmentIntent): Map<string, string[]> {
  const owned = new Set(intent.services.map((service) => service.lineageId));
  const users = new Map<string, string[]>();
  for (const service of intent.services) {
    for (const variable of service.variables) {
      if (variable.value.kind !== "template") continue;
      for (const part of variable.value.parts) {
        if (part.kind !== "ref" || part.owner.scope !== "service" || owned.has(part.owner.lineageId)) continue;
        const using = users.get(part.owner.lineageId) ?? [];
        if (!using.includes(service.id)) using.push(service.id);
        users.set(part.owner.lineageId, using);
      }
    }
  }
  return users;
}
