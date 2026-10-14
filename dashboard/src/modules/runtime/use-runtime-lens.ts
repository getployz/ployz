import { useCollectionScope } from "#/collections/use-collection-scope";
import { useLiveQuery } from "@tanstack/react-db";
import { useHydrated } from "@tanstack/react-router";
import {
  getRuntimeCollections,
  isIncompleteObservation,
  projectRuntimeMachineRecord,
} from "#/modules/runtime/runtime.collection";

export function useRuntimeLens(organizationSlug: string) {
  const collections = getRuntimeCollections(organizationSlug, useCollectionScope());
  const liveMachines = useLiveQuery(collections.machines);
  const liveStatus = useLiveQuery(collections.status);
  const hydrated = useHydrated();
  const machines = hydrated ? (liveMachines.data ?? []) : [];
  const statusRows = hydrated ? (liveStatus.data ?? []) : [];
  const status = statusRows[0]?.status ?? "connecting";
  const error = statusRows[0]?.error ?? null;
  const isLoading = hydrated && (liveMachines.isLoading || liveStatus.isLoading);

  return {
    machines: machines.map(projectRuntimeMachineRecord),
    /** Cloud knows the Organization has no Server to run anything: none connected, or none observed. */
    noServers: status === "no_connection" || (status === "observed" && machines.length === 0),
    status,
    error,
    incomplete: statusRows[0] ? isIncompleteObservation(statusRows[0].incompleteIds) : false,
    /** When the evidence shown was current; kept when the connection drops, null before any. */
    observedAt: statusRows[0]?.observedAt ?? null,
    /** Each managed Volume copy the Servers reported, with its role. */
    volumeCopies: statusRows[0]?.volumeCopies ?? [],
    isLoading,
  };
}
