export type RuntimeStatus = "disabled" | "connecting" | "live" | "error";

import type { RuntimeServiceRecord } from "#/modules/runtime/runtime.collection";

export type {
  RuntimeRouteBinding,
  RuntimeServiceRecord,
} from "#/modules/runtime/runtime.collection";

/** The hostnames the managed (automatic-origin) bindings are serving for a
 * service — the single source for managed-domain drift detection. */
export function automaticBoundHostnames(
  runtime: RuntimeServiceRecord | null | undefined,
): string[] {
  return (
    runtime?.bindings
      .filter((binding) => binding.origin === "automatic")
      .map((binding) => binding.hostname) ?? []
  );
}
