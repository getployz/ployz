import { queryOptions } from "@tanstack/react-query";
import { Schema } from "effect";
import type { RuntimeContainerRecord } from "./runtime.collection";

/** A Machine that has sent no logs for this long reads as not reporting. */
export const NOT_REPORTING_MS = 5 * 60_000;
const STATUS_POLL_MS = 30_000;

export const logHistoryStatusSchema = Schema.Struct({
  history: Schema.Boolean,
  now: Schema.optionalKey(Schema.Number),
  machines: Schema.Array(Schema.Struct({ machineId: Schema.String, seenAt: Schema.Number })),
});
export type LogHistoryStatus = typeof logHistoryStatusSchema.Type;

/** Whether this Cloud keeps log history, and when each Machine last sent any. */
export function logHistoryStatusQueryOptions(organizationSlug: string) {
  return queryOptions({
    queryKey: ["log-history-status", organizationSlug] as const,
    queryFn: async ({ signal }) => {
      const response = await fetch(`/api/runtime/log-history-status?${new URLSearchParams({ organizationSlug })}`, { signal });
      if (!response.ok) throw new Error("Could not read the log history status.");
      return Schema.decodeUnknownSync(logHistoryStatusSchema)(await response.json());
    },
    staleTime: STATUS_POLL_MS,
    refetchInterval: STATUS_POLL_MS,
  });
}

export type SilentMachine = { readonly machineId: string; readonly silentMs: number | null };
const NONE: readonly SilentMachine[] = [];

/**
 * What is off about a Service's logs, from where its containers run. A container still on Docker's `local` driver
 * is one a 0.3 daemon started: its logs never reach Cloud until it is redeployed, so its Machine is not also called
 * quiet. A container whose driver was not reported is neither. A Machine whose running containers send through the Collector but has sent nothing for five minutes is.
 */
export function logOffStates(input: {
  status: LogHistoryStatus | undefined;
  containers: readonly RuntimeContainerRecord[];
  now: number;
}) {
  const { status, containers } = input;
  if (status === undefined) return { onLocal: false, silent: NONE };
  const now = status.now ?? input.now;
  const seen = new Map(status.machines.map((machine) => [machine.machineId, machine.seenAt]));
  const running = containers.filter((container) => container.runtime?.state === "running");
  const onLocal = running.some((container) => container.logDriver === "local");
  const sending = [...new Set(running.filter((container) => container.logDriver !== undefined && container.logDriver !== "local").map((container) => container.machineId))];
  const silent = !status.history ? NONE : sending.flatMap((machineId): SilentMachine[] => {
    const at = seen.get(machineId);
    if (at === undefined) return [{ machineId, silentMs: null }];
    return now - at > NOT_REPORTING_MS ? [{ machineId, silentMs: now - at }] : [];
  });
  return { onLocal: status.history && onLocal, silent };
}
