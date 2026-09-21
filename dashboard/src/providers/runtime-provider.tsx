import { useCollectionScope } from "#/collections/use-collection-scope";
import { createContext, use, useEffect } from "react";
import { eq, useLiveQuery } from "@tanstack/react-db";
import { Option, Schema } from "effect";
import {
  applyRuntimeSnapshot,
  CLUSTER_UNREACHABLE_ERROR,
  getCachedRuntimeSnapshot,
  getRuntimeCollections,
  noConnectionRuntimeSnapshot,
  projectRuntimeServiceRecord,
  unavailableRuntimeSnapshot,
  unreachableRuntimeSnapshot,
  EMPTY_RUNTIME_INCOMPLETE_IDS,
  type RuntimeCollections,
} from "#/modules/runtime/runtime.collection";
import {
  runtimeConnectionStatusEventSchema,
  runtimeSnapshotFromWatchFrame,
  runtimeWatchFrameSchema,
} from "#/modules/runtime/runtime-watch-frame";
import { buildRuntimeEventsUrl } from "#/providers/runtime-events-url";

type RuntimeContextValue = { collections: RuntimeCollections };

const RuntimeContext = createContext<RuntimeContextValue | null>(null);

export function RuntimeProvider({
  organizationSlug,
  children,
}: {
  organizationSlug: string;
  children: React.ReactNode;
}) {
  const collections = getRuntimeCollections(organizationSlug, useCollectionScope());

  useEffect(() => {
    const eventSource = new EventSource(buildRuntimeEventsUrl(organizationSlug));
    let expectIntentionalClose = false;

    // Keep the last direct observation visible after an EventSource failure.
    // A later Runtime Watch event replaces it atomically.
    const applyUnavailable = (error: string) => {
      const snapshot = unavailableRuntimeSnapshot(
        getCachedRuntimeSnapshot(collections),
        error,
      );
      applyRuntimeSnapshot({ collections, snapshot });
    };

    const handleWatch = (event: MessageEvent) => {
      let raw: unknown;
      try {
        raw = JSON.parse(event.data);
      } catch {
        applyUnavailable("Runtime observation could not be read.");
        return;
      }
      const parsed = Schema.decodeUnknownOption(runtimeWatchFrameSchema)(raw);
      if (Option.isNone(parsed)) {
        applyUnavailable("Runtime observation could not be read.");
        return;
      }
      const snapshot = runtimeSnapshotFromWatchFrame(parsed.value);
      expectIntentionalClose = false;
      applyRuntimeSnapshot({ collections, snapshot });
    };

    const handleStatus = (event: MessageEvent) => {
      let raw: unknown;
      try {
        raw = JSON.parse(event.data);
      } catch {
        applyUnavailable("Runtime connection state could not be read.");
        return;
      }
      const parsed = Schema.decodeUnknownOption(
        runtimeConnectionStatusEventSchema,
      )(raw);
      if (Option.isNone(parsed)) {
        applyUnavailable("Runtime connection state could not be read.");
        return;
      }

      const snapshot =
        parsed.value.status === "no_connection"
          ? noConnectionRuntimeSnapshot()
          : unreachableRuntimeSnapshot(
              parsed.value.error ?? CLUSTER_UNREACHABLE_ERROR,
            );
      expectIntentionalClose = true;
      applyRuntimeSnapshot({ collections, snapshot });
    };

    // EventSource reconnects itself. Its error event only changes the
    // connection state; it never fabricates a replacement observation.
    const handleError = () => {
      if (expectIntentionalClose) {
        expectIntentionalClose = false;
        return;
      }
      applyUnavailable("Runtime connection lost.");
    };

    eventSource.addEventListener("runtime.watch", handleWatch);
    eventSource.addEventListener("runtime.status", handleStatus);
    eventSource.addEventListener("error", handleError);

    return () => {
      eventSource.removeEventListener("runtime.watch", handleWatch);
      eventSource.removeEventListener("runtime.status", handleStatus);
      eventSource.removeEventListener("error", handleError);
      eventSource.close();
    };
  }, [organizationSlug, collections]);

  return (
    <RuntimeContext.Provider value={{ collections }}>
      {children}
    </RuntimeContext.Provider>
  );
}

function useRuntimeContext() {
  const context = use(RuntimeContext);
  if (!context) throw new Error("Runtime hooks must be used within RuntimeProvider");
  return context;
}

export function useRuntimeStatus() {
  const { collections } = useRuntimeContext();
  const { data: rows = [] } = useLiveQuery(collections.status);
  const row = rows[0];
  const lensStatus = row?.status ?? "connecting";

  return {
    lensStatus,
    error: row?.error ?? null,
    // These are direct Runtime Watch fields. They deliberately do not imply
    // DNS publication, route binding, certificate use, or service health.
    hostedDnsHostname: row?.hostedDnsHostname ?? null,
    certificates: row?.certificates ?? [],
    incompleteIds: row?.incompleteIds ?? EMPTY_RUNTIME_INCOMPLETE_IDS,
  };
}

/** Read a direct Engine Service grouping by its exact `project/name` identity. */
export function useRuntimeService(identity: string) {
  const { collections } = useRuntimeContext();
  const { data: rows = [] } = useLiveQuery({
    queryKey: ['runtime-service', collections.services.id, identity],
    query: (q) =>
      q
        .from({ service: collections.services })
        .where(({ service }) => eq(service.identity, identity))
        .select(({ service }) => service),
  });

  return {
    runtime: rows[0] ? projectRuntimeServiceRecord(rows[0]) : null,
  };
}
