export type Placed = {
  readonly key: string;
  readonly name: string;
  readonly environment: string;
  /**
   * The region keys its Service runs in, in any case: the regions it sets its replicas by, else the one its Environment
   * names. Empty where it runs in any region.
   */
  readonly regions: readonly string[];
  /** `regions` are the ones it counts its replicas in, so a Server in one of them holds that region's replica. */
  readonly perRegion: boolean;
  /** Its Environment's Working State couldn't be read, so where its Services run next is unknown, not any region. */
  readonly unread: boolean;
};

/** A Placed that leaves on the next Deploy; `to` names the Servers in its regions it can run on. */
export type Leaving = Placed & { readonly to: readonly string[] };

/** An Environment's Namespace as the Store names it, with where its Services run. */
type OwnedNamespace = {
  readonly namespace: string;
  readonly project: string;
  readonly environment: string;
  readonly region?: string;
  readonly unread?: boolean;
  /** The regions each Service that sets its replicas by region runs in, whatever the Environment names. */
  readonly replica_regions?: Readonly<Record<string, readonly string[]>>;
};

/**
 * Each Service on a Server with its Environment (`project/environment`) and the regions it runs in. A Service no
 * Environment owns names neither, so it never leaves.
 */
export function placedServices(
  services: readonly { readonly identity: string; readonly name: string; readonly namespace: string | null }[],
  owned: readonly OwnedNamespace[],
): Placed[] {
  return services.map(({ identity, name, namespace }) => {
    const owner = owned.find((each) => each.namespace === namespace);
    const own = owner?.replica_regions?.[name];
    return {
      key: identity,
      name,
      environment: owner === undefined ? namespace ?? "" : `${owner.project}/${owner.environment}`,
      regions: own ?? (owner?.region === undefined ? [] : [owner.region]),
      perRegion: own !== undefined,
      unread: owner?.unread === true,
    };
  });
}

/** The Services whose Environment couldn't be read, by Environment: what a region change can't say it moves. */
export function unreadEnvironments(placed: readonly Placed[]): string[] {
  return [...new Set(placed.filter(({ unread }) => unread).map(({ environment }) => environment))].sort();
}

/**
 * What leaves the Server `serverId` once it is in region `to` (null: no region). A Service that runs in one region
 * leaves when `to` isn't it. A Service that counts replicas by region leaves too when the Server holds the replica of
 * the region it is in now, which it stops doing once `to` is another, though `to` is counted as well. Region keys
 * compare without case. `to` on each lists the other Servers that take Services and are not offline, in the regions
 * the Service must run in, where a Deploy can move it.
 */
export function leavingServer(
  serverId: string,
  to: string | null,
  placed: readonly Placed[],
  servers: readonly {
    readonly id: string;
    readonly name: string;
    readonly region: string | null;
    readonly acceptsServices: boolean;
    readonly offline: boolean;
  }[],
): Leaving[] {
  const here = to?.toLowerCase();
  const from = servers.find(({ id }) => id === serverId)?.region?.toLowerCase();
  return placed.flatMap((item) => {
    const regions = item.regions.map((region) => region.toLowerCase());
    if (regions.length === 0) return [];
    const heldRegion = item.perRegion && from !== undefined && regions.includes(from) && from !== here ? from : null;
    if (heldRegion === null && here !== undefined && regions.includes(here)) return [];
    const wanted = heldRegion === null ? regions : [heldRegion];
    const there = servers.filter((other) =>
      other.id !== serverId && other.acceptsServices && !other.offline && wanted.includes(other.region?.toLowerCase() ?? ""));
    return [{ ...item, to: there.map(({ name }) => name).sort() }];
  });
}
