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
  return response.status === 200 && (await response.text()).trim() === machineId ? "ployz" : "taken";
}).pipe(Effect.orElseSucceed((): IngressAnswer => "closed"));
