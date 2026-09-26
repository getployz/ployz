import { createCollection, localOnlyCollectionOptions } from "@tanstack/react-db";
import { Schema } from "effect";
import { cachedByCollectionScope, type CollectionScope } from "#/collections/scope";
import { appendContainerLogs, containerLogEventSchema, containerLogPageSchema, mergeContainerHistory, remainingHistory, type ContainerLogRow } from "./container-log.collection";

export type ContainerLogSelection = { organizationSlug: string; environmentSlug?: string; deploymentId?: string; serviceId?: string };

/**
 * `opened`: the server has answered once, so an empty log means no output rather than not loaded yet.
 * `offline`: the organization's servers are unreachable, the one state the viewer can act on.
 */
type LogStreamState = { opened: boolean; offline: boolean; errors: Record<string, string>; historyPending: boolean; historyError: boolean };

const INITIAL: LogStreamState = { opened: false, offline: false, errors: {}, historyPending: false, historyError: false };
const MAX_RETRY_MS = 30_000;

function createLogStream(id: string, selection: ContainerLogSelection, scope: CollectionScope) {
  let snapshot = INITIAL;
  const listeners = new Set<() => void>();
  const publish = (next: typeof snapshot) => { snapshot = next; listeners.forEach(listener => listener()); };
  const exhausted: Record<string, string> = {};
  let controller = new AbortController();
  const query = new URLSearchParams(Object.entries(selection).filter((entry): entry is [string, string] => entry[1] !== undefined)).toString();
  const options = localOnlyCollectionOptions({ id, getKey: (row: ContainerLogRow) => row.id, initialData: [] });
  const collection = createCollection({
    ...options,
    startSync: false,
    gcTime: 300_000,
    sync: {
      sync(params) {
        const local = options.sync.sync(params);
        let events: EventSource | undefined;
        let retry: ReturnType<typeof setTimeout> | undefined;
        let retryMs = 1_000;
        const close = () => { clearTimeout(retry); events?.close(); controller.abort(); };
        // The browser retries a dropped stream by itself, quietly; the rows' ids drop the tail it replays.
        // Only a refused one (an error response) closes for good, so that one retries here, backing off.
        const connect = () => {
          controller = new AbortController();
          events = new EventSource(`/api/runtime/logs?${query}`);
          events.addEventListener("live", () => { retryMs = 1_000; publish({ ...snapshot, opened: true, offline: false }); });
          events.addEventListener("offline", () => { retryMs = 1_000; publish({ ...snapshot, opened: true, offline: true }); });
          events.onerror = () => {
            if (events?.readyState !== EventSource.CLOSED) return;
            close();
            retry = setTimeout(connect, retryMs);
            retryMs = Math.min(retryMs * 2, MAX_RETRY_MS);
          };
          events.addEventListener("log", (event: MessageEvent<string>) => {
            const decoded = Schema.decodeUnknownOption(Schema.fromJsonString(containerLogEventSchema))(event.data);
            if (decoded._tag === "None") return;
            if (decoded.value.type === "record") appendContainerLogs(collection, [decoded.value.record]);
            else publish({ ...snapshot, errors: { ...snapshot.errors, [`${decoded.value.machineId}/${decoded.value.containerId}`]: decoded.value.message } });
          });
        };
        // Logs stream only in the browser; SSR renders the loading state.
        if (!import.meta.env.SSR) connect();
        return () => {
          close();
          for (const source of Object.keys(exhausted)) delete exhausted[source];
          publish(INITIAL);
          // The library explicitly returns a cleanup function or a cleanup handle.
          // oxlint-disable-next-line anti-slop/no-runtime-typeof
          if (typeof local === "function") local(); else local?.cleanup?.();
        };
      },
    },
  });
  async function loadOlder() {
    if (snapshot.historyPending) return;
    const before = remainingHistory([...collection.values()], exhausted);
    if (!Object.keys(before).length) return;
    const streamSignal = controller.signal;
    publish({ ...snapshot, historyPending: true, historyError: false });
    try {
      const page = await scope.queryClient.fetchQuery({
        queryKey: [id, "history", before],
        // Logs older than a fixed boundary never change.
        staleTime: Infinity,
        queryFn: async ({ signal }) => {
          const response = await fetch(`/api/runtime/logs?${query}&before=${encodeURIComponent(JSON.stringify(before))}`, { signal: AbortSignal.any([signal, streamSignal]) });
          if (!response.ok) throw new Error("Could not load older logs.");
          return Schema.decodeUnknownSync(containerLogPageSchema)(await response.json());
        },
      });
      streamSignal.throwIfAborted();
      mergeContainerHistory(collection, page.records);
      for (const [source, boundary] of Object.entries(before)) {
        const failed = page.errors.some(error => `${error.machineId}/${error.containerId}` === source);
        const progressed = page.records.some(row => `${row.machineId}/${row.containerId}` === source && BigInt(row.timestamp) < BigInt(boundary));
        if (!failed && !progressed) exhausted[source] = boundary;
      }
      publish({ ...snapshot, errors: Object.fromEntries(page.errors.map(error => [`${error.machineId}/${error.containerId}`, error.message])), historyPending: false, historyError: page.errors.length > 0 });
    } catch {
      if (!streamSignal.aborted) publish({ ...snapshot, historyPending: false, historyError: true });
    }
  }
  return {
    collection, loadOlder,
    get signal() { return controller.signal; },
    getSnapshot: () => snapshot,
    subscribe: (listener: () => void) => { listeners.add(listener); return () => { listeners.delete(listener); }; },
  };
}

const streams = cachedByCollectionScope(() => new Map<string, ReturnType<typeof createLogStream>>());
export function getContainerLogStream(selection: ContainerLogSelection, scope: CollectionScope) {
  const cache = streams(selection.organizationSlug, scope);
  const key = JSON.stringify([selection.environmentSlug, selection.deploymentId, selection.serviceId]);
  let stream = cache.get(key);
  if (!stream) {
    stream = createLogStream(`container-logs:${scope.sessionId}:${scope.userId}:${selection.organizationSlug}:${key}`, selection, scope);
    cache.set(key, stream);
  }
  return stream;
}
