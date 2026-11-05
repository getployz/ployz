import { queryCollectionOptions, type QueryCollectionConfig, type QueryCollectionUtils } from "@tanstack/query-db-collection";
import { BasicIndex, collectionOptions, type Collection, type CollectionStatus } from "@tanstack/react-db";
import { environmentManager, hashKey, type QueryClient } from "@tanstack/react-query";
import type { CollectionRead } from "./read.contract";
import { getDbClient } from "./scope";
import { mayPassOnRetry } from "#/lib/error-message";

type ApiCollectionInput<T> = {
  queryClient: QueryClient;
  queryKey: readonly string[];
  getKey: (row: T) => string | number;
  staleTime?: number;
};

type BaseOptions<T extends object> = Pick<QueryCollectionConfig<T>, "queryClient" | "queryKey" | "getKey" | "id" | "startSync"
  | "staleTime" | "refetchOnWindowFocus" | "refetchOnReconnect" | "retry" | "autoIndex" | "defaultIndexType">;

function baseOptions<T extends object>(input: ApiCollectionInput<T>): BaseOptions<T> {
  // Default snapshot retention lets a loader hand data to its consumer after releasing its observer.
  return {
    queryClient: input.queryClient,
    queryKey: input.queryKey,
    getKey: input.getKey,
    id: input.queryKey.join(":"),
    startSync: false,
    staleTime: input.staleTime ?? 15_000,
    refetchOnWindowFocus: "always",
    refetchOnReconnect: "always",
    // A refresh retries with backoff while its rows stand in. A first load fails at once, so its gate shows the error.
    retry: (failures, error) => failures < 3 && mayPassOnRetry(error) && input.queryClient.getQueryData(input.queryKey) !== undefined,
    autoIndex: "eager",
    defaultIndexType: BasicIndex,
  };
}

/** Each SSR request's collections, by its QueryClient. The browser keeps its collections for the tab's life. */
const requestCollections = new WeakMap<QueryClient, Set<Pick<Collection<object>, "cleanup">>>();

/**
 * Ends an SSR request's collections at once. A collection a page still reads re-reads its Query when the Query is
 * removed, so they end before the request's QueryClient is cleared: a read then would run outside the request.
 */
export function endRequestCollections(queryClient: QueryClient) {
  const collections = requestCollections.get(queryClient);
  requestCollections.delete(queryClient);
  for (const collection of collections ?? []) void collection.cleanup({ dependents: true }).catch(() => {});
}

/**
 * A failed first load ends its sync run, and later reads of the same Query never reach it. The Collection reloads
 * after its Query next succeeds, or when `recover()` is called. A reload starts a new run, which cancels every read of
 * the old run still settling, so it waits until none is, holding the Collection meanwhile. A cleanup or another run
 * during the wait ends the recovery. The Query cache is watched only while the Collection is in `error`.
 */
function withRecovery<T extends object, C extends Pick<Collection<T>, "status" | "reload" | "on" | "subscribeChanges" | "cleanup">>(
  input: ApiCollectionInput<T>, collection: C,
) {
  if (environmentManager.isServer()) {
    const collections = requestCollections.get(input.queryClient) ?? new Set();
    requestCollections.set(input.queryClient, collections.add(collection));
  }
  const queryHash = hashKey(input.queryKey);
  const fetching = () => input.queryClient.getQueryCache().get(queryHash)?.state.fetchStatus === "fetching";
  const recover = async () => {
    let ended = false;
    const stop = collection.on("status:change", () => {
      ended = true;
    });
    let hold: { unsubscribe: () => void } | undefined;
    try {
      do {
        // Subscribing to a failed Collection reads its ended run, so it holds only onto a read already running.
        if (!hold && fetching()) hold = collection.subscribeChanges(() => {});
        await input.queryClient.getQueryCache().get(queryHash)?.promise?.catch(() => {});
        // A refetch's caller settles in the microtasks after its read, and a reload before then cancels it.
        await new Promise((resolve) => setTimeout(resolve));
      } while (fetching());
    } finally {
      stop();
      // Released before the reload: its run would stop watching its own read when the last subscriber leaves.
      hold?.unsubscribe();
    }
    if (!ended && collection.status === "error") await collection.reload();
  };
  let watching: (() => void) | undefined;
  const watch = () => input.queryClient.getQueryCache().subscribe((event) => {
    if (event.type !== "updated" || event.action.type !== "success" || event.query.queryHash !== queryHash) return;
    if (collection.status === "error") void recover().catch(() => {});
  });
  const arm = () => collection.on("status:change", ({ status }) => {
    if (status === "error") watching ??= watch();
    else {
      watching?.();
      watching = undefined;
    }
    // Cleanup drops the collection's listeners right after this event, and a later start may fail again.
    if (status === "cleaned-up") queueMicrotask(rearm);
  });
  let disarm = arm();
  const rearm = () => {
    disarm();
    disarm = arm();
  };
  return Object.assign(collection, { recover });
}

/** Each owner supplies its request-local QueryClient and authenticated scope. A Remote Read that polls sets `refetchInterval`. */
export function createApiCollection<T extends object>(input: ApiCollectionInput<T> & {
  refetchInterval?: number;
  queryFn: (context: { signal: AbortSignal }) => Promise<T[]>;
}) {
  const options = queryCollectionOptions({ ...baseOptions(input), refetchInterval: input.refetchInterval, queryFn: input.queryFn });
  return withRecovery(input, getDbClient(input.queryClient).collection(collectionOptions(options)));
}

/** Rows and their change cursor live in one Query entry, so a cancelled or reverted read reverts both. */
export type ChangeSnapshot<T> = { rows: T[]; cursor?: string };

/**
 * TanStack DB's incremental pattern for Query collections: read `since` the cached cursor,
 * merge into the cached rows, and return the complete list. A full read replaces them.
 * No timer: the Organization change stream refetches it, as do focus and reconnect.
 */
export function createChangeCollection<T extends object>(input: ApiCollectionInput<T> & {
  read: (context: { signal: AbortSignal; since: string | undefined }) => Promise<CollectionRead<T>>;
}) {
  const queryFn = async ({ signal }: { signal: AbortSignal }): Promise<ChangeSnapshot<T>> => {
    const previous = input.queryClient.getQueryData<ChangeSnapshot<T>>(input.queryKey);
    const result = await input.read({ signal, since: previous?.cursor });
    if (result.full) return { rows: result.rows, cursor: result.cursor };
    const rows = new Map((previous?.rows ?? []).map((row) => [String(input.getKey(row)), row]));
    // Deletes first: a key deleted and re-created within the window comes back as a row.
    for (const key of result.deleted) rows.delete(key);
    for (const row of result.rows) rows.set(String(input.getKey(row)), row);
    return { rows: [...rows.values()], cursor: result.cursor };
  };
  const options = queryCollectionOptions({
    ...baseOptions(input),
    queryFn,
    select: (snapshot: ChangeSnapshot<T>) => snapshot.rows,
  });
  return withRecovery(input, getDbClient(input.queryClient).collection(collectionOptions(options)));
}

type ApiCollectionReadiness = Pick<Collection<object>, "status" | "preload"> & {
  recover: () => Promise<void>;
  subscribeChanges: (callback: () => void) => { unsubscribe: () => void };
  utils: Pick<QueryCollectionUtils, "refetch">;
};

const runningStatuses: readonly CollectionStatus[] = ["loading", "error"];

/** Hold an observer only until readiness; a failed first read rejects and recovers on the next call. */
export async function preloadCollection(collection: ApiCollectionReadiness) {
  if (collection.status === "error") {
    await collection.recover();
    // A run another caller started during recovery owns readiness, and a cleaned-up Collection stays cleaned up.
    if (runningStatuses.includes(collection.status)) await collection.preload();
    return;
  }
  const subscription = collection.subscribeChanges(() => {});
  try {
    await collection.preload();
  } finally {
    subscription.unsubscribe();
  }
}

/** Readiness of several collections. It fails only once every read settles, since SSR streams a read still running and its later cancel rejects in the browser. */
export async function preloadCollections(collections: ApiCollectionReadiness[]) {
  const reads = await Promise.allSettled(collections.map(preloadCollection));
  const failed = reads.find((read) => read.status === "rejected");
  if (failed) throw failed.reason;
}

/** Secondary freshness: Query owns read errors after the command has committed. */
export async function reconcileCollection(collection: ApiCollectionReadiness) {
  const subscription = collection.subscribeChanges(() => {});
  try {
    await collection.preload();
    await collection.utils.refetch();
  } finally {
    subscription.unsubscribe();
  }
}
