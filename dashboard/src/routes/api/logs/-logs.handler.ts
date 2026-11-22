import "@tanstack/react-start/server-only";
import { Effect, Option, Schema } from "effect";
import { resolveCaller } from "#/modules/identity/caller.server";
import { resolveLogFilter } from "#/modules/runtime/container-logs.server";
import { MAX_SEARCH_LENGTH, readLogPage } from "#/modules/runtime/log-history-page.server";
import { clickhouseEndpoint, OBSERVE_NOT_CONFIGURED } from "#/modules/observe-store/clickhouse.server";
import { logHistoryCursor } from "#/modules/runtime/log-history.server";
import { lacks } from "#/modules/organization/access.server";
import { readCappedBody } from "#/server/capped-body";
import { Forbidden, NotFound, Unavailable, Validation } from "#/server/public-error";

const MAX_REQUEST_BYTES = 16 * 1024;
const DEFAULT_LIMIT = 200;
const MAX_LIMIT = 1_000;

const logsReadSchema = Schema.Struct({
  environment: Schema.Struct({ project: Schema.String, environment: Schema.String }),
  /** One Service by its runtime name, which a removed Service still has in the stored lines. */
  service: Schema.optional(Schema.String.check(Schema.isMinLength(1), Schema.isMaxLength(200))),
  machine: Schema.optional(Schema.String.check(Schema.isPattern(/^[0-9a-f]{32}$/))),
  search: Schema.optional(Schema.String.check(Schema.isMinLength(1), Schema.isMaxLength(MAX_SEARCH_LENGTH))),
  limit: Schema.optional(Schema.Int.check(Schema.isBetween({ minimum: 1, maximum: MAX_LIMIT }))),
  cursor: Schema.optional(logHistoryCursor),
});

/**
 * `POST /api/logs/read`: the newest stored lines of an Environment's Services as the calling Organization, newest
 * first, with a `cursor` for the older page. It reads Cloud's own history, so a removed Service's lines read the
 * same as a running one's, and needs no Machine connected.
 */
export const handleLogsRead = Effect.fn("Runtime.handleLogsRead")(function* (request: Request) {
  if (request.method !== "POST") return yield* new NotFound({ message: "Not found." });
  const caller = yield* resolveCaller(request.headers);
  const lacking = lacks(caller, "logs:read");
  if (lacking !== null) return yield* new Forbidden({ message: lacking });
  const bytes = yield* Effect.tryPromise(() => readCappedBody(request, MAX_REQUEST_BYTES)).pipe(Effect.orElseSucceed(() => null));
  const input: unknown = bytes === null ? undefined : yield* Effect.try({
    try: () => JSON.parse(new TextDecoder().decode(bytes)),
    catch: () => new Validation({ message: "Expected a JSON body.", userFacing: true }),
  });
  const query = Schema.decodeUnknownOption(logsReadSchema)(input);
  if (Option.isNone(query)) return yield* new Validation({ message: "Expected an environment, and optionally a service, machine, search, limit and cursor.", userFacing: true });
  const endpoint = yield* clickhouseEndpoint;
  if (endpoint === undefined) return yield* new Unavailable({ message: OBSERVE_NOT_CONFIGURED, userFacing: true });
  const { environment, service, machine, search, limit, cursor } = query.value;
  const organizationId = caller.organization.id;
  const filter = yield* resolveLogFilter(organizationId, {
    organizationSlug: caller.organization.slug, projectSlug: environment.project, environmentSlug: environment.environment,
  });
  const page = yield* readLogPage(endpoint, {
    organizationId, namespace: filter.namespace, serviceName: service, machineId: machine, search,
  }, { limit: limit ?? DEFAULT_LIMIT, cursor });
  return Response.json({
    lines: page.rows.map(({ row, machineName }) => ({
      timestamp: row.timestamp, machineId: row.machineId, machineName,
      containerId: row.containerId, service: row.serviceName, stream: row.stream, message: row.message,
    })),
    cursor: page.cursor,
  }, { headers: { "cache-control": "no-store" } });
});
