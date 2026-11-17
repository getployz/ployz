export type Placed = {
  readonly key: string;
  readonly name: string;
  readonly volume: string | null;
  readonly environment: string;
  /** The region key its Environment names, in any case; null where it names none. */
  readonly region: string | null;
};

/** A Placed that leaves on the next Deploy; `to` names the Servers in its region it can run on. */
export type Leaving = Placed & { readonly to: readonly string[] };

export function placedServices(services: readonly { readonly identity: string; readonly name: string; readonly namespace: string | null }[]): Placed[] {
  return services.map(({ identity, name, namespace }) => ({ key: identity, name, volume: null, environment: namespace ?? "", region: null }));
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
