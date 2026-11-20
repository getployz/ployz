import "@tanstack/react-start/server-only";
import { and, eq, inArray, isNotNull } from "drizzle-orm";
import { Effect, Option, Schema } from "effect";
import { rustMachineIdSchema } from "#/modules/machines/enrollment";
import { organizationMachine } from "#/modules/machines/tables";
import { authorizeRequest } from "#/modules/organization/access.server";
import { organizationSlugSchema } from "#/modules/organization/tables";
import { Database } from "#/server/database.server";
import { Forbidden } from "#/server/public-error";
import type { ContainerLogLine } from "./container-log.collection";
import { logSearchSchema, resolveLogFilter } from "./container-logs.server";
import { clickhouseEndpoint } from "#/modules/observe-store/clickhouse.server";
import { type LogHistoryRow, logHistoryCursor, readLogHistory } from "./log-history.server";
import { logLevel } from "./log-level";

const HISTORY_PAGE = 500;
export const MAX_SEARCH_LENGTH = 200;

/** A read of the history Cloud keeps: the newest page of a selection's lines, or the one behind `cursor`, optionally those holding `contains`. */
export const logHistorySchema = Schema.Struct({
  ...logSearchSchema.fields,
  cursor: Schema.optional(logHistoryCursor),
  contains: Schema.optional(Schema.String.check(Schema.isMinLength(1), Schema.isMaxLength(MAX_SEARCH_LENGTH))),
  machineId: Schema.optional(Schema.String.check(Schema.isPattern(/^[0-9a-f]{32}$/))),
});
export type LogHistoryRequest = typeof logHistorySchema.Type;

export const historyStatusSchema = Schema.Struct({ organizationSlug: organizationSlugSchema });

export function projectHistoryRow(row: LogHistoryRow, machineName: string): ContainerLogLine {
  return {
    kind: "line", id: `${row.machineId}/${row.containerId}/${row.timestamp}/h${row.hash}`, timestamp: row.timestamp,
    machineId: row.machineId, machineName, containerId: row.containerId, serviceName: row.serviceName,
    channel: row.stream, level: logLevel(row.message), message: row.message,
  };
}

const decodeMachineId = Schema.decodeUnknownOption(rustMachineIdSchema);

export const machineNames = Effect.fn("Runtime.historyMachineNames")(function* (organizationId: string, machineIds: ReadonlyArray<string>) {
  const known = machineIds.flatMap((id) => Option.toArray(decodeMachineId(id)));
  if (known.length === 0) return new Map<string, string>();
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.select({ machineId: organizationMachine.machineId, machineName: organizationMachine.machineName })
    .from(organizationMachine)
    .where(and(eq(organizationMachine.organizationId, organizationId), inArray(organizationMachine.machineId, known)));
  return new Map(rows.flatMap((row) => row.machineName === null ? [] : [[row.machineId, row.machineName] as const]));
});

const authorizeHistory = Effect.fn("Runtime.authorizeHistory")(function* (request: Request, organizationSlug: string) {
  const access = yield* authorizeRequest(request.headers, organizationSlug, "logs:read");
  if (access.kind === "support" && !access.shows.logs) {
    return yield* new Forbidden({ message: "This Organization doesn't show its container logs to Ployz support." });
  }
  return access.organization.id;
});

export const readHistoryPage = Effect.fn("Runtime.readHistoryPage")(function* (request: Request, input: LogHistoryRequest) {
  const organizationId = yield* authorizeHistory(request, input.organizationSlug);
  const endpoint = yield* clickhouseEndpoint;
  if (endpoint === undefined) return { rows: [], failures: [], cursor: null };
  const filter = yield* resolveLogFilter(organizationId, input);
  const page = yield* readLogHistory(endpoint, {
    organizationId, namespace: filter.namespace, serviceName: filter.serviceName,
    machineId: input.machineId, search: input.contains,
  }, { limit: HISTORY_PAGE, cursor: input.cursor });
  const names = yield* machineNames(organizationId, [...new Set(page.rows.map((row) => row.machineId))]);
  return {
    rows: page.rows.map((row) => projectHistoryRow(row, names.get(row.machineId) ?? row.machineId)),
    failures: [],
    cursor: page.cursor,
  };
});

/**
 * Whether this Cloud keeps log history, and when each Machine that has sent anything last did, by Cloud's own clock
 * (`now`) so the viewer's clock cannot make a Machine look quiet. The viewer decides from it which Machines of a Service
 * have gone quiet.
 */
export const readHistoryStatus = Effect.fn("Runtime.readHistoryStatus")(function* (request: Request, organizationSlug: string) {
  const organizationId = yield* authorizeHistory(request, organizationSlug);
  const endpoint = yield* clickhouseEndpoint;
  if (endpoint === undefined) return { history: false, now: Date.now(), machines: [] };
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.select({ machineId: organizationMachine.machineId, seenAt: organizationMachine.observeSeenAt })
    .from(organizationMachine)
    .where(and(eq(organizationMachine.organizationId, organizationId), isNotNull(organizationMachine.observeSeenAt)));
  return { history: true, now: Date.now(), machines: rows.flatMap((row) => row.seenAt === null ? [] : [{ machineId: row.machineId, seenAt: row.seenAt.getTime() }]) };
});
