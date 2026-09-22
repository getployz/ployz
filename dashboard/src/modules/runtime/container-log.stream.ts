import { createCollection, localOnlyCollectionOptions } from "@tanstack/react-db";
import { Schema } from "effect";
import { cachedByCollectionScope, type CollectionScope } from "#/collections/scope";
import { appendContainerLogs, containerLogEventSchema, type ContainerLogRow } from "./container-log.collection";

export type ContainerLogSelection = { organizationSlug: string; environmentSlug?: string; deploymentId?: string; serviceId?: string };

type LogStreamState = { status: string; errors: Record<string, string> };

function createLogStream(id: string, selection: ContainerLogSelection) {
  let snapshot: LogStreamState = { status: "Connecting…", errors: {} };
  const listeners = new Set<() => void>();
  const publish = (next: typeof snapshot) => { snapshot = next; listeners.forEach(listener => listener()); };
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
          publish({ status: "Connecting…", errors: {} });
          events = new EventSource(`/api/runtime/logs?${query}`);
          events.onopen = () => publish({ ...snapshot, status: "Live" });
          const fail = () => { events?.close(); publish({ ...snapshot, status: "Log connection ended. Refresh to reconnect." }); };
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
          // The library explicitly returns a cleanup function or a cleanup handle.
          // oxlint-disable-next-line anti-slop/no-runtime-typeof
          if (typeof local === "function") local(); else local?.cleanup?.();
        };
      },
    },
  });
  return {
    collection, query,
    get signal() { return controller.signal; },
    refresh: () => reconnect(),
    getSnapshot: () => snapshot,
    subscribe: (listener: () => void) => { listeners.add(listener); return () => { listeners.delete(listener); }; },
    setErrors: (errors: Record<string, string>) => publish({ ...snapshot, errors }),
  };
}

const streams = cachedByCollectionScope(() => new Map<string, ReturnType<typeof createLogStream>>());
export function getContainerLogStream(selection: ContainerLogSelection, scope: CollectionScope) {
  const cache = streams(selection.organizationSlug, { ...scope, environmentSlug: undefined });
  const key = JSON.stringify([selection.environmentSlug, selection.deploymentId, selection.serviceId]);
  let stream = cache.get(key);
  if (!stream) {
    stream = createLogStream(`container-logs:${scope.sessionId}:${scope.userId}:${selection.organizationSlug}:${key}`, selection);
    cache.set(key, stream);
  }
  return stream;
}
