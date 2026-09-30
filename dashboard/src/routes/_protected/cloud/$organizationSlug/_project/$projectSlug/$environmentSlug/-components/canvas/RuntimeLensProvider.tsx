import { createContext, type ReactNode } from "react";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import type { RuntimeLens } from "./node-status";

/**
 * What the Runtime Watch says of the Servers, read once for the canvas. Its cards and bottom bar read it here, so a
 * runtime frame re-renders only them, never the canvas around them. Without a provider it is still connecting.
 */
export const RuntimeLensContext = createContext<RuntimeLens>({ status: "connecting", incomplete: false, observedAt: null, noServers: false });

export function RuntimeLensProvider({ organizationSlug, children }: { organizationSlug: string; children: ReactNode }) {
  return <RuntimeLensContext value={useRuntimeLens(organizationSlug)}>{children}</RuntimeLensContext>;
}
