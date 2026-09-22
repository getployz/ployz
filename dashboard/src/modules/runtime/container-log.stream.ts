import { createCollection, localOnlyCollectionOptions } from "@tanstack/react-db";
import { Schema } from "effect";
import { cachedByCollectionScope, type CollectionScope } from "#/collections/scope";
import { appendContainerLogs, containerLogEventSchema, containerLogPageSchema, mergeContainerHistory, remainingHistory, type ContainerLogRow } from "./container-log.collection";

export type ContainerLogSelection = { organizationSlug: string; environmentSlug?: string; deploymentId?: string; serviceId?: string };

type LogStreamState = { status: string; errors: Record<string, string>; historyPending: boolean; historyError: boolean };

function createLogStream(id: string, selection: ContainerLogSelection, scope: CollectionScope) {
  let snapshot: LogStreamState = { status: "Connecting…", errors: {}, historyPending: false, historyError: false };
  const listeners = new Set<() => void>();
  const publish = (next: typeof snapshot) => { snapshot = next; listeners.forEach(listener => listener()); };
  const exhausted: Record<string, string> = {};
  let controller = new AbortController();
  let reconnect = () => {};
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
        const close = () => { events?.close(); controller.abort(); };
        reconnect = () => {
          close();
          controller = new AbortController();
          publish({ ...snapshot, status: "Connecting…", errors: {}, historyPending: false, historyError: false });
          events = new EventSource(`/api/runtime/logs?${query}`);
          events.onopen = () => publish({ ...snapshot, status: "Live" });
          const fail = () => { events?.close(); publish({ ...snapshot, status: "Disconnected" }); };
          events.onerror = fail;
          events.addEventListener("unavailable", fail);
          events.addEventListener("log", (event: MessageEvent<string>) => {
            try {
              const decoded = Schema.decodeUnknownSync(Schema.fromJsonString(containerLogEventSchema))(event.data);
              if (decoded.type === "record") appendContainerLogs(collection, [decoded.record]);
              else publish({ ...snapshot, errors: { ...snapshot.errors, [`${decoded.machineId}/${decoded.containerId}`]: decoded.message } });
            } catch { fail(); }
          });
        };
        reconnect();
        return () => {
          close(); reconnect = () => {};
          for (const source of Object.keys(exhausted)) delete exhausted[source];
          publish({ status: "Connecting…", errors: {}, historyPending: false, historyError: false });
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
    refresh: () => reconnect(),
    getSnapshot: () => snapshot,
    subscribe: (listener: () => void) => { listeners.add(listener); return () => { listeners.delete(listener); }; },
  };
}

const streams = cachedByCollectionScope(() => new Map<string, ReturnType<typeof createLogStream>>());
export function getContainerLogStream(selection: ContainerLogSelection, scope: CollectionScope) {
  const cache = streams(selection.organizationSlug, { ...scope, environmentSlug: undefined });
  const key = JSON.stringify([selection.environmentSlug, selection.deploymentId, selection.serviceId]);
  let stream = cache.get(key);
  if (!stream) {
    stream = createLogStream(`container-logs:${scope.sessionId}:${scope.userId}:${selection.organizationSlug}:${key}`, selection, scope);
    cache.set(key, stream);
  }
  return stream;
}
