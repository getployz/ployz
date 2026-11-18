export type Placed = {
  readonly key: string;
  readonly name: string;
  readonly environment: string;
  /** The region key its Environment names, in any case; null where it names none. */
  readonly region: string | null;
};

/** A Placed that leaves on the next Deploy; `to` names the Servers in its region it can run on. */
export type Leaving = Placed & { readonly to: readonly string[] };

/** An Environment's Namespace as the Store names it, with the region the Environment runs in. */
type OwnedNamespace = { readonly namespace: string; readonly project: string; readonly environment: string; readonly region?: string };

/**
 * Each Service on a Server with its Environment (`project/environment`) and that Environment's region. A Service no
 * Environment owns names neither, so it never leaves.
 */
export function placedServices(
  services: readonly { readonly identity: string; readonly name: string; readonly namespace: string | null }[],
  owned: readonly OwnedNamespace[],
): Placed[] {
  return services.map(({ identity, name, namespace }) => {
    const owner = owned.find((each) => each.namespace === namespace);
    return {
      key: identity,
      name,
      environment: owner === undefined ? namespace ?? "" : `${owner.project}/${owner.environment}`,
      region: owner?.region ?? null,
    };
  });
}

/**
 * What leaves the Server `serverId` once it is in region `to` (null: no region): the `placed` whose Environment
 * names another region. Region keys compare without case.
 */
export function leavingServer(
  serverId: string,
  to: string | null,
  placed: readonly Placed[],
  servers: readonly { readonly id: string; readonly name: string; readonly region: string | null; readonly acceptsServices: boolean }[],
): Leaving[] {
  return placed.flatMap((item) => {
    const region = item.region?.toLowerCase();
    if (region === undefined || region === to?.toLowerCase()) return [];
    const there = servers.filter((other) => other.id !== serverId && other.acceptsServices && other.region?.toLowerCase() === region);
    return [{ ...item, to: there.map(({ name }) => name).sort() }];
  });
}
