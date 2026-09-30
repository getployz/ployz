import { createContext, use, type ReactNode } from "react";
import { useLoaderData, useParams } from "@tanstack/react-router";
import { namespaceQuery, useStoreView } from "#/modules/config-store/store-view.queries";
import type { RuntimeVolumeRecord } from "#/modules/runtime/runtime.collection";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import { ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";
import { dockerVolumeName, volumeFill, type RuntimeLens } from "./node-status";

/**
 * What the Runtime Watch says of the Servers, read once for the canvas. Its cards and bottom bar read it here, so a
 * runtime frame re-renders only them, never the canvas around them. Without a provider it is still connecting.
 */
export const RuntimeLensContext = createContext<RuntimeLens & { volumes: readonly RuntimeVolumeRecord[] }>({ status: "connecting", incomplete: false, observedAt: null, noServers: false, volumes: [] });

export function RuntimeLensProvider({ organizationSlug, children }: { organizationSlug: string; children: ReactNode }) {
  return <RuntimeLensContext value={useRuntimeLens(organizationSlug)}>{children}</RuntimeLensContext>;
}

/** How full a Volume here is, by its id, as `volumeFill` reads it; none when the Environment has no Namespace to name it by. */
export function useVolumeFill() {
  const { organizationSlug } = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { store } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const namespace = useStoreView(organizationSlug, namespaceQuery(store));
  const { volumes } = use(RuntimeLensContext);
  return (volumeId: string) => namespace.ok ? volumeFill(volumes, dockerVolumeName(namespace.value.namespace, volumeId)) : null;
}
