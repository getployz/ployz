import "@tanstack/react-start/server-only";
import { Effect } from "effect";
import type { MachineId } from "#/db/tables";

const PROBE_TIMEOUT_MS = 5_000;
/** Caddy answers it on port 80 of every ingress Server with the Machine id (core `INGRESS_VERIFY_PATH`). */
const INGRESS_VERIFY_PATH = "/.ployz-verify";

/** Who answers on port 80: Ployz with the Machine id, something else, or nothing within five seconds. */
export type IngressAnswer = "ployz" | "taken" | "closed";

export const probeIngress = (origin: string, machineId: MachineId) => Effect.tryPromise(async (signal): Promise<IngressAnswer> => {
  const response = await fetch(`${origin}${INGRESS_VERIFY_PATH}`, {
    redirect: "manual",
    signal: AbortSignal.any([signal, AbortSignal.timeout(PROBE_TIMEOUT_MS)]),
  });
  if (response.status !== 200) return "taken";
  // A response that arrived proves something answers, even if its body then fails.
  const body = await response.text().catch(() => "");
  return body.trim() === machineId ? "ployz" : "taken";
}).pipe(Effect.orElseSucceed((): IngressAnswer => "closed"));
