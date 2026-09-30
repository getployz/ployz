import { createContext, type ReactNode } from "react";
import { useLoaderData } from "@tanstack/react-router";
import { namespaceQuery, useStoreView } from "#/modules/config-store/store-view.queries";
import type { RuntimeVolumeRecord } from "#/modules/runtime/runtime.collection";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import { ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";
import type { RuntimeLens } from "./node-status";

/**
 * What the Runtime Watch says of the Servers, read once for the canvas, with the Environment's Namespace, which names
 * its containers and Volumes there (null when it has none). Its cards and bottom bar read it here, so a runtime frame
 * re-renders only them, never the canvas around them. Without a provider it is still connecting.
 */
export const RuntimeLensContext = createContext<RuntimeLens & { volumes: readonly RuntimeVolumeRecord[]; namespace: string | null }>(
  { status: "connecting", incomplete: false, observedAt: null, noServers: false, volumes: [], namespace: null });

export function RuntimeLensProvider({ organizationSlug, children }: { organizationSlug: string; children: ReactNode }) {
  const { store } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  // The route loads it before the canvas, so this never suspends.
  const namespace = useStoreView(organizationSlug, namespaceQuery(store));
  const lens = useRuntimeLens(organizationSlug);
  return <RuntimeLensContext value={{ ...lens, namespace: namespace.ok ? namespace.value.namespace : null }}>{children}</RuntimeLensContext>;
}
