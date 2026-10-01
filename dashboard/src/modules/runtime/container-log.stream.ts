import { createCollection, localOnlyCollectionOptions } from "@tanstack/react-db";
import { Schema } from "effect";
import { cachedByCollectionScope, type CollectionScope } from "#/collections/scope";
import { liveStream } from "#/lib/live.stream";
import { appendContainerLogs, containerLogEventSchema, containerLogPageSchema, mergeContainerHistory, remainingHistory, trimContainerLogs, type ContainerLogRow } from "./container-log.collection";

export type ContainerLogSelection = { organizationSlug: string; projectSlug?: string; environmentSlug?: string; deploymentId?: string; serviceId?: string };

/**
 * `opened`: the server has answered once, so an empty log means no output rather than not loaded yet.
 * `offline`: the organization's servers are unreachable, the one state the viewer can act on.
 * `refused`: the last connect got an error response; it keeps retrying until one opens.
 */
type LogStreamState = { opened: boolean; offline: boolean; refused: boolean; errors: Record<string, string>; historyPending: boolean; historyError: boolean };

const INITIAL: LogStreamState = { opened: false, offline: false, refused: false, errors: {}, historyPending: false, historyError: false };

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
        // Lines arrive one event each; land them in batches so a noisy service doesn't re-render the page per line.
        let pending: ContainerLogRow[] = [];
        let flush: ReturnType<typeof setTimeout> | undefined;
        const land = () => { flush = undefined; appendContainerLogs(collection, pending); pending = []; trimContainerLogs(collection); };
        // A replayed tail after a reconnect is dropped by the rows' ids. A refused stream ends its history reads.
        controller = new AbortController();
        const stop = liveStream(`/api/runtime/logs?${query}`, {
          onOpen: () => { if (controller.signal.aborted) controller = new AbortController(); },
          // Any loss says so, so a first connection that fails never sits on the loading skeleton; only a refused one
          // also ends its history reads.
          onLost: (why) => {
            if (why === "refused") controller.abort();
            if (!snapshot.offline && !snapshot.refused) publish({ ...snapshot, refused: true });
          },
          on: {
            live: () => publish({ ...snapshot, opened: true, offline: false, refused: false }),
            offline: () => publish({ ...snapshot, opened: true, offline: true, refused: false }),
            log: (event) => {
              const decoded = Schema.decodeUnknownOption(Schema.fromJsonString(containerLogEventSchema))(event.data);
              if (decoded._tag === "None") return;
              if (decoded.value.type === "record") { pending.push(decoded.value.record); flush ??= setTimeout(land, 250); }
              else publish({ ...snapshot, errors: { ...snapshot.errors, [`${decoded.value.machineId}/${decoded.value.containerId}`]: decoded.value.message } });
            },
          },
        });
        const close = () => { clearTimeout(flush); flush = undefined; pending = []; stop(); controller.abort(); };
        return () => {
          pending = [];
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
    get hasOlder() { return Object.keys(remainingHistory([...collection.values()], exhausted)).length > 0; },
    get signal() { return controller.signal; },
    getSnapshot: () => snapshot,
    subscribe: (listener: () => void) => { listeners.add(listener); return () => { listeners.delete(listener); }; },
  };
}

const streams = cachedByCollectionScope(() => new Map<string, ReturnType<typeof createLogStream>>());
export function getContainerLogStream(selection: ContainerLogSelection, scope: CollectionScope) {
  const cache = streams(selection.organizationSlug, scope);
  const key = JSON.stringify([selection.projectSlug, selection.environmentSlug, selection.deploymentId, selection.serviceId]);
  let stream = cache.get(key);
  if (!stream) {
    stream = createLogStream(`container-logs:${scope.sessionId}:${scope.userId}:${selection.organizationSlug}:${key}`, selection, scope);
    cache.set(key, stream);
  }
  return stream;
}
