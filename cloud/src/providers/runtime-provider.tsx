import { createContext, use, useEffect, useRef } from "react";
import { eq, useLiveQuery } from "@tanstack/react-db";
import { Option, Schema } from "effect";
import {
  applyRuntimeSnapshot,
  getCachedRuntimeSnapshot,
  getRuntimeCollections,
  projectRuntimeMachineRecord,
  runtimeSnapshotLensSchema,
  unavailableRuntimeSnapshot,
  type RuntimeCollections,
  type RuntimeSnapshotLens,
} from "#/modules/runtime/runtime.collection";
import { buildRuntimeEventsUrl } from "#/providers/runtime-events-url";
import type { RuntimeServiceRecord, RuntimeStatus } from "#/modules/runtime/runtime";

type RuntimeContextValue = {
  collections: RuntimeCollections;
};

const RuntimeContext = createContext<RuntimeContextValue | null>(null);

export function RuntimeProvider({
  organizationSlug,
  children,
}: {
  organizationSlug: string;
  children: React.ReactNode;
}) {
  const collections = getRuntimeCollections({ organizationSlug });

  const lastSnapshotRef = useRef<RuntimeSnapshotLens | null>(null);

  useEffect(() => {
    lastSnapshotRef.current = getCachedRuntimeSnapshot({ organizationSlug });
    const eventSource = new EventSource(buildRuntimeEventsUrl(organizationSlug));

    let expectIntentionalClose = false;

    // Keep the last machines/services and only flip status to unavailable; a
    // later valid snapshot replaces all state. Used for EventSource failure and
    // for malformed/invalid runtime events.
    const applyUnavailable = (error: string) => {
      const snapshot = unavailableRuntimeSnapshot(lastSnapshotRef.current, error);
      lastSnapshotRef.current = snapshot;
      applyRuntimeSnapshot({ organizationSlug, snapshot });
    };

    const handleLens = (event: MessageEvent) => {
      let raw: unknown;
      try {
        raw = JSON.parse(event.data);
      } catch {
        applyUnavailable("Runtime snapshot could not be read.");
        return;
      }
      const parsed = Schema.decodeUnknownOption(runtimeSnapshotLensSchema)(raw, {
        onExcessProperty: "error",
      });
      if (Option.isNone(parsed)) {
        applyUnavailable("Runtime snapshot could not be read.");
        return;
      }
      lastSnapshotRef.current = parsed.value;
      expectIntentionalClose =
        parsed.value.status === "no_connection" ||
        parsed.value.status === "unreachable";
      applyRuntimeSnapshot({ organizationSlug, snapshot: parsed.value });
    };

    // EventSource auto-reconnects. On failure we keep the last machines/services
    // and only flip status to unavailable; a later snapshot replaces all state.
    const handleError = () => {
      // Swallow the single expected close that follows a no_connection or
      // unreachable status and let EventSource auto-reconnect. Reset so a
      // subsequent reconnect error still surfaces as unavailable.
      if (expectIntentionalClose) {
        expectIntentionalClose = false;
        return;
      }
      applyUnavailable("Runtime connection lost.");
    };

    eventSource.addEventListener("runtime.lens", handleLens);
    eventSource.addEventListener("error", handleError);

    return () => {
      eventSource.removeEventListener("runtime.lens", handleLens);
      eventSource.removeEventListener("error", handleError);
      eventSource.close();
    };
  }, [organizationSlug]);

  return (
    <RuntimeContext.Provider value={{ collections }}>
      {children}
    </RuntimeContext.Provider>
  );
}

function useRuntimeContext() {
  const context = use(RuntimeContext);
  if (!context) {
    throw new Error("Runtime hooks must be used within RuntimeProvider");
  }
  return context;
}

export function useRuntimeStatus() {
  const { collections } = useRuntimeContext();
  const { data: rows = [] } = useLiveQuery({
    query: (q) =>
      q.from({ status: collections.status }).select(({ status }) => status),
  });
  const row = rows[0];

  return {
    status: runtimeStatusFromLens(row?.status ?? "connecting"),
    lensStatus: row?.status ?? "connecting",
    error: row?.error ?? null,
  };
}

/** Cluster-level managed public-URL state: the mode and the auto-domain suffix
 * (null until the lease is acquired). Available even before any service deploys. */
export function useRuntimePublicUrl() {
  const { collections } = useRuntimeContext();
  const { data: rows = [] } = useLiveQuery({
    query: (q) =>
      q.from({ status: collections.status }).select(({ status }) => status),
  });
  const row = rows[0];

  const publicUrl = row?.publicUrl;
  return {
    mode: publicUrl?.mode ?? "disabled",
    autoDomain: publicUrl?.domain ?? null,
    leaseApex: publicUrl?.leaseApex ?? null,
    dnsTarget: publicUrl?.dnsTarget ?? {
      intent: "disabled",
      allocation: "unacquired",
      publication: "unpublished",
    },
  };
}

export function useRuntimeMachines() {
  const { collections } = useRuntimeContext();
  const { data: machines = [], isLoading, isReady } = useLiveQuery({
    query: (q) =>
      q.from({ machine: collections.machines }).select(({ machine }) => machine),
  });

  return {
    machines: machines.map(projectRuntimeMachineRecord),
    isLoading: isLoading || !isReady,
  };
}

export function useRuntimeService(environmentNamespace: string, serviceId: string) {
  const { collections } = useRuntimeContext();
  const { data: rows = [], isLoading, isReady } = useLiveQuery({
    query: (q) =>
      q
        .from({ service: collections.services })
        .where(({ service }) =>
          eq(service.namespaceId, environmentNamespace),
        )
        .where(({ service }) =>
          eq(service.serviceId, serviceId),
        )
        .select(({ service }) => service),
  });

  return {
    runtime: rows[0] ?? null,
    isLoading: isLoading || !isReady,
  };
}

export function useRuntimeServices(environmentNamespace: string) {
  const { collections } = useRuntimeContext();
  const { data: rows = [], isLoading, isReady } = useLiveQuery({
    query: (q) =>
      q
        .from({ service: collections.services })
        .where(({ service }) =>
          eq(service.namespaceId, environmentNamespace),
        )
        .select(({ service }) => service),
  });

  return {
    runtimeServices: rows,
    isLoading: isLoading || !isReady,
  };
}

function runtimeStatusFromLens(
  status: RuntimeSnapshotLens["status"],
): RuntimeStatus {
  switch (status) {
    case "no_connection":
    case "live_empty":
      return "disabled";
    case "connecting":
      return "connecting";
    case "live_rows":
      return "live";
    case "unavailable":
    case "unreachable":
      return "error";
    default: {
      const exhaustive: never = status;
      return exhaustive;
    }
  }
}

export type { RuntimeServiceRecord };
