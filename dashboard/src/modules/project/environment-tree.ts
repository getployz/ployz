/** One Environment in the tree: its depth under a root and the Parent it was branched from. */
export type EnvironmentTreeNode<E> = { environment: E; depth: number; parent: E | null };

/**
 * A project's Environments in tree order: root Environments first, each Branch right under its Parent, siblings oldest
 * first. A Branch whose Parent isn't in `environments` reads as a root.
 */
export function environmentTree<E extends { id: string; createdAt: Date }>(
  environments: Iterable<E>,
  branches: Iterable<{ environmentId: string; parentEnvironmentId: string }>,
): EnvironmentTreeNode<E>[] {
  const sorted = [...environments].sort((a, b) => a.createdAt.getTime() - b.createdAt.getTime());
  const byId = new Map(sorted.map((environment) => [environment.id, environment]));
  const parentOf = new Map<string, E>();
  for (const branch of branches) {
    const parent = byId.get(branch.parentEnvironmentId);
    if (parent) parentOf.set(branch.environmentId, parent);
  }
  const tree: EnvironmentTreeNode<E>[] = [];
  const visit = (parent: E | null, depth: number) => {
    for (const environment of sorted) {
      if ((parentOf.get(environment.id) ?? null) !== parent) continue;
      tree.push({ environment, depth, parent });
      visit(environment, depth + 1);
    }
  };
  visit(null, 0);
  return tree;
}

/** The ids of every Branch under `environmentId`, deepest first: the order they close in. */
export function descendants(
  environmentId: string,
  branches: Iterable<{ environmentId: string; parentEnvironmentId: string }>,
): string[] {
  const rows = [...branches];
  const found: string[] = [];
  let level = [environmentId];
  while (level.length > 0) {
    level = rows
      .filter((branch) => level.includes(branch.parentEnvironmentId) && !found.includes(branch.environmentId))
      .map((branch) => branch.environmentId);
    found.unshift(...level);
  }
  return found;
}

/** `environmentId` and then each Environment it was branched from, nearest first; its last entry is the root. */
export function ancestors(
  environmentId: string,
  branches: Iterable<{ environmentId: string; parentEnvironmentId: string }>,
): string[] {
  const parentOf = new Map([...branches].map((branch) => [branch.environmentId, branch.parentEnvironmentId]));
  const found: string[] = [];
  for (let at: string | undefined = environmentId; at && !found.includes(at); at = parentOf.get(at)) found.push(at);
  return found;
}
