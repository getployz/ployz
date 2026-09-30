import { createContext, use, type ReactNode } from "react";
import type { RuntimeVolumeRecord } from "#/modules/runtime/runtime.collection";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import { dockerVolumeName, volumeFill, type RuntimeLens } from "./node-status";

/**
 * What the Runtime Watch says of the Servers, read once for the canvas. Its cards and bottom bar read it here, so a
 * runtime frame re-renders only them, never the canvas around them. Without a provider it is still connecting.
 */
export const RuntimeLensContext = createContext<RuntimeLens & { volumes: readonly RuntimeVolumeRecord[] }>({ status: "connecting", incomplete: false, observedAt: null, noServers: false, volumes: [] });

export function RuntimeLensProvider({ organizationSlug, children }: { organizationSlug: string; children: ReactNode }) {
  return <RuntimeLensContext value={useRuntimeLens(organizationSlug)}>{children}</RuntimeLensContext>;
}

/** How full a Volume here is, by its id, as `volumeFill` reads it. Without a provider, none reports. */
export const VolumeFillContext = createContext<(volumeId: string) => number | null>(() => null);

/** The Environment's Volume fills; `namespace`, null when the Environment has none, so nothing names its Volumes. */
export function VolumeFillProvider({ namespace, children }: { namespace: string | null; children: ReactNode }) {
  const { volumes } = use(RuntimeLensContext);
  const fillOf = (volumeId: string) => namespace === null ? null : volumeFill(volumes, dockerVolumeName(namespace, volumeId));
  return <VolumeFillContext value={fillOf}>{children}</VolumeFillContext>;
}
