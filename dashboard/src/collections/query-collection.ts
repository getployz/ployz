import { queryCollectionOptions, type QueryCollectionConfig, type QueryCollectionUtils } from "@tanstack/query-db-collection";
import { BasicIndex, collectionOptions, type Collection } from "@tanstack/react-db";
import type { QueryClient } from "@tanstack/react-query";
import type { CollectionRead } from "./read.contract";
import { getDbClient } from "./scope";

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
    retry: false,
    autoIndex: "eager",
    defaultIndexType: BasicIndex,
  };
}

type ApiCollection<T extends object> = Pick<Collection<T, string | number, QueryCollectionUtils<T>>, "subscribeChanges" | "utils">;

function withWriteCommitted<T extends object, C extends ApiCollection<T>>(input: ApiCollectionInput<T>, collection: C) {
  return Object.assign(collection, {
    async writeCommitted(rows: T | T[]): Promise<void> {
      const subscription = collection.subscribeChanges(() => {});
      try {
        // A read started before the POST must not overwrite its committed response.
        await input.queryClient.cancelQueries({ queryKey: input.queryKey, exact: true });
        collection.utils.writeUpsert(rows);
      } finally {
        subscription.unsubscribe();
      }
    },
  });
}

/** Each owner supplies its request-local QueryClient and authenticated scope. A Remote Read that polls sets `refetchInterval`. */
export function createApiCollection<T extends object>(input: ApiCollectionInput<T> & {
  refetchInterval?: number;
  queryFn: (context: { signal: AbortSignal }) => Promise<T[]>;
}) {
  const options = queryCollectionOptions({ ...baseOptions(input), refetchInterval: input.refetchInterval, queryFn: input.queryFn });
  return withWriteCommitted(input, getDbClient(input.queryClient).collection(collectionOptions(options)));
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
  return withWriteCommitted(input, getDbClient(input.queryClient).collection(collectionOptions(options)));
}

type ApiCollectionReadiness = {
  preload: () => Promise<void>;
  subscribeChanges: (callback: () => void) => { unsubscribe: () => void };
  utils: Pick<QueryCollectionUtils, "refetch" | "isError" | "lastError" | "dataUpdatedAt">;
};

/** Hold an observer only until readiness; failed first reads are not empty snapshots. */
export async function preloadCollection(collection: ApiCollectionReadiness) {
  const retryInitialFailure = collection.utils.isError && collection.utils.dataUpdatedAt === 0;
  const subscription = collection.subscribeChanges(() => {});
  try {
    if (retryInitialFailure) await collection.utils.refetch({ throwOnError: true });
    else await collection.preload();
    if (collection.utils.isError && collection.utils.dataUpdatedAt === 0) {
      throw collection.utils.lastError;
    }
  } finally {
    subscription.unsubscribe();
  }
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

export type Persistable = { isPersisted: { promise: Promise<unknown> } };

/** Failures are toasted where they happen; observing them keeps fire-and-forget callers free of unhandled rejections. */
export function observeFailure<T extends Persistable>(transaction: T): T {
  transaction.isPersisted.promise.catch(() => {});
  return transaction;
}
