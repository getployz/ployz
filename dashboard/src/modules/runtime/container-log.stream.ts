import { createCollection, localOnlyCollectionOptions } from "@tanstack/react-db";
import { Schema } from "effect";
import { cachedByCollectionScope, type CollectionScope } from "#/collections/scope";
import { liveStream } from "#/lib/live.stream";
import { appendContainerLogs, containerLogEventSchema, containerLogPageSchema, LIVE_LOG_LIMIT, mergeContainerHistory, restartContainerLogs, trimContainerLogs, type ContainerLogRow, type MissingServer } from "./container-log.collection";

export type ContainerLogSelection = { organizationSlug: string; projectSlug?: string; environmentSlug?: string; deploymentId?: string; serviceId?: string };

/**
 * `opened`: the server has answered once, so an empty log means no output rather than not loaded yet.
 * `offline`: the organization's servers are unreachable, the one state the viewer can act on.
 * `refused`: the last connect got an error response; it keeps retrying until one opens.
 * `missing`: Servers whose logs aren't here, by ID, with why; the live stream's and the last history page's.
 */
type LogStreamState = {
  opened: boolean; offline: boolean; refused: boolean;
  missing: { live: Record<string, Omit<MissingServer, "machineId"> & { containerIds: readonly string[] }>; history: readonly MissingServer[] };
  historyPending: boolean; historyError: boolean;
};

const INITIAL: LogStreamState = { opened: false, offline: false, refused: false, missing: { live: {}, history: [] }, historyPending: false, historyError: false };
/** Fewer lines than this from the live tail, as an old Deployment's stopped containers give, reads the Log Store at once. */
const SHORT_TAIL = 200;
/** Pages whose lines the live tail already had, read before giving the viewer older ones. */
const OVERLAP_PAGES = 4;
/** Scrollback stops growing here, until the viewer follows the newest lines again and the page trims it. */
const SCROLLBACK_LIMIT = LIVE_LOG_LIMIT * 3;

function createLogStream(id: string, selection: ContainerLogSelection, scope: CollectionScope) {
  let snapshot = INITIAL;
  const listeners = new Set<() => void>();
  const publish = (next: typeof snapshot) => { snapshot = next; listeners.forEach(listener => listener()); };
  // The Log Store's position: not read yet, a cursor, or null once it has nothing older.
  let cursor: string | null | undefined;
  const stored = new Set<string>();
  // Bumped by each reset, so a read begun before it lands nothing.
  let generation = 0;
  const resetHistory = () => {
    cursor = undefined;
    stored.clear();
    generation++;
  };
  // Scrolled up, live lines wait so the ones being read stay put; past the limit the oldest waiting go.
  let following = true;
  let overflowed = false;
  let land = () => {};
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
        land = () => {
          clearTimeout(flush);
          flush = undefined;
          if (!following) {
            // Servers' lines interleave out of order, so the ones kept are the newest by time, not the last to arrive.
            if (pending.length > LIVE_LOG_LIMIT) {
              pending = [...pending].sort((a, b) => (BigInt(a.timestamp) < BigInt(b.timestamp) ? -1 : BigInt(a.timestamp) > BigInt(b.timestamp) ? 1 : 0)).slice(-LIVE_LOG_LIMIT);
              overflowed = true;
            }
            return;
          }
          // The Store's lines that outlive the restart still dedupe the waiting ones; only then does history start over.
          if (overflowed) restartContainerLogs(collection, pending);
          appendContainerLogs(collection, pending, stored);
          if (overflowed) {
            resetHistory();
            overflowed = false;
          }
          pending = [];
          // The oldest lines go first, so a trim takes the scrollback and the Log Store reads again from the new oldest.
          if (trimContainerLogs(collection) && cursor !== undefined) resetHistory();
        };
        // A replayed tail after a reconnect is dropped by the rows' ids. A refused stream ends its history reads.
        // A container that failed is sending again or has left; the note stays while another on that Server is still down.
        const recovered = (machineId: string, containerId: string) => {
          const server = snapshot.missing.live[machineId];
          if (!server?.containerIds.includes(containerId)) return;
          const { [machineId]: _, ...live } = snapshot.missing.live;
          const containerIds = server.containerIds.filter(failed => failed !== containerId);
          publish({ ...snapshot, missing: { ...snapshot.missing, live: containerIds.length ? { ...live, [machineId]: { ...server, containerIds } } : live } });
        };
        controller = new AbortController();
        const stop = liveStream(`/api/runtime/logs?${query}`, {
          onOpen: () => { if (controller.signal.aborted) controller = new AbortController(); },
          // Any loss says so, so a first connection that fails never sits on the loading skeleton; only a refused one
          // also ends its history reads.
          onLost: (why) => {
            if (why === "refused") {
              controller.abort();
              publish({ ...snapshot, historyPending: false });
            }
            if (!snapshot.offline && !snapshot.refused) publish({ ...snapshot, refused: true });
          },
          on: {
            live: () => {
              publish({ ...snapshot, opened: true, offline: false, refused: false });
              land();
              if (cursor === undefined && collection.size < SHORT_TAIL) void loadOlder();
            },
            offline: () => publish({ ...snapshot, opened: true, offline: true, refused: false }),
            log: (event) => {
              const decoded = Schema.decodeUnknownOption(Schema.fromJsonString(containerLogEventSchema))(event.data);
              if (decoded._tag === "None") return;
              if (decoded.value.type === "record") {
                recovered(decoded.value.record.machineId, decoded.value.record.containerId);
                pending.push(decoded.value.record); flush ??= setTimeout(land, 250);
              } else if (decoded.value.type === "source_gone") {
                recovered(decoded.value.machineId, decoded.value.containerId);
              } else {
                const { machineId, containerId, message } = decoded.value;
                const machineName = [...collection.values()].find(row => row.machineId === machineId)?.machineName ?? machineId;
                const containerIds = [...(snapshot.missing.live[machineId]?.containerIds ?? []).filter(failed => failed !== containerId), containerId];
                publish({ ...snapshot, missing: { ...snapshot.missing, live: { ...snapshot.missing.live, [machineId]: { machineName, containerIds, message } } } });
              }
            },
          },
        });
        const close = () => { clearTimeout(flush); flush = undefined; pending = []; stop(); controller.abort(); };
        return () => {
          pending = [];
          land = () => {};
          following = true;
          overflowed = false;
          close();
          resetHistory();
          publish(INITIAL);
          // The library explicitly returns a cleanup function or a cleanup handle.
          // oxlint-disable-next-line anti-slop/no-runtime-typeof
          if (typeof local === "function") local(); else local?.cleanup?.();
        };
      },
    },
  });
  async function loadOlder() {
    if (controller.signal.aborted || snapshot.historyPending || cursor === null || collection.size >= SCROLLBACK_LIMIT) return;
    const streamSignal = controller.signal;
    const begun = generation;
    const cancelHistory = () => { void scope.queryClient.cancelQueries({ queryKey: [id, "history"] }); };
    streamSignal.addEventListener("abort", cancelHistory, { once: true });
    publish({ ...snapshot, historyPending: true, historyError: false });
    try {
      const oldest = () => earliest(collection.values());
      const reached = oldest();
      let failures: readonly MissingServer[] = [];
      // Each read follows the last page's cursor; a page the live tail already had dedupes by its lines' ids.
      for (let read = 0; read < OVERLAP_PAGES && cursor !== null; read++) {
        const from = cursor;
        const page: typeof containerLogPageSchema.Type = await scope.queryClient.fetchQuery({
          queryKey: [id, "history", from ?? null],
          staleTime: query => from !== undefined && query.state.data?.failures.length === 0 ? Infinity : 0,
          queryFn: async ({ signal }) => {
            const response = await fetch("/api/runtime/logs", {
              method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ ...selection, cursor: from }), signal: AbortSignal.any([signal, streamSignal]),
            });
            if (!response.ok) throw new Error("Could not load older logs.");
            return Schema.decodeUnknownSync(containerLogPageSchema)(await response.json());
          },
        });
        streamSignal.throwIfAborted();
        if (generation !== begun) break;
        mergeContainerHistory(collection, page.rows, stored);
        cursor = page.cursor;
        failures = page.failures;
        const now = oldest();
        if (failures.length || reached === null || (now !== null && now < reached)) break;
      }
      publish({ ...snapshot, missing: { ...snapshot.missing, history: failures }, historyPending: false });
    } catch {
      if (!streamSignal.aborted) publish({ ...snapshot, historyPending: false, historyError: true });
    } finally {
      streamSignal.removeEventListener("abort", cancelHistory);
    }
  }
  return {
    collection, loadOlder,
    /** Whether the viewer is at the newest line; live lines land only then. */
    follow(at: boolean) {
      if (at === following) return;
      following = at;
      if (at) land();
    },
    get hasOlder() { return cursor !== null; },
    get signal() { return controller.signal; },
    getSnapshot: () => snapshot,
    subscribe: (listener: () => void) => { listeners.add(listener); return () => { listeners.delete(listener); }; },
  };
}

function earliest(rows: Iterable<ContainerLogRow>) {
  let min: bigint | null = null;
  for (const row of rows) if (min === null || BigInt(row.timestamp) < min) min = BigInt(row.timestamp);
  return min;
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
