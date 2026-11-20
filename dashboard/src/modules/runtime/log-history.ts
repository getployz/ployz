import { queryOptions } from "@tanstack/react-query";
import { Schema } from "effect";
import type { RuntimeContainerRecord } from "./runtime.collection";

export const NOT_REPORTING_MS = 5 * 60_000;
const STATUS_POLL_MS = 30_000;

export const logHistoryStatusSchema = Schema.Struct({
  history: Schema.Boolean,
  now: Schema.optionalKey(Schema.Number),
  machines: Schema.Array(Schema.Struct({ machineId: Schema.String, seenAt: Schema.Number })),
});
export type LogHistoryStatus = typeof logHistoryStatusSchema.Type;

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
