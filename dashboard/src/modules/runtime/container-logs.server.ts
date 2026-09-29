import "@tanstack/react-start/server-only";
import { Effect, Exit, Schema, Scope } from "effect";
import type { ConfigQuery, LogFilter } from "@ployz/sdk";
import { authorizeRuntimeOrganization } from "./authorize-runtime-organization.server";
import { OrganizationRuntime } from "./organization-runtime.server";
import { backfillThenFollow } from "./container-log-events.server";
import { NotFound, Validation } from "#/server/public-error";
import { organizationSlugSchema } from "#/modules/organization/tables";
import { readStore } from "#/modules/config-store/config-store.server";

export const logSearchSchema = Schema.Struct({
  organizationSlug: organizationSlugSchema,
  environmentSlug: Schema.optional(Schema.String),
  /** An Environment is named within its Project. */
  projectSlug: Schema.optional(Schema.String),
  deploymentId: Schema.optional(Schema.String.check(Schema.isUUID())),
  serviceId: Schema.optional(Schema.String.check(Schema.isUUID())),
  before: Schema.optional(Schema.fromJsonString(Schema.Record(Schema.String, Schema.String.check(Schema.isPattern(/^-?\d{1,19}$/))))),
});
export type LogSearch = typeof logSearchSchema.Type;

/**
 * Whose logs: a Deployment's (its Namespace, then the containers labelled with its ID), or an Environment's (its
 * Namespace). The Store answers for the Organization only, so another's stays not found.
 */
export const resolveLogFilter = Effect.fn("Runtime.resolveLogFilter")(function* (organizationId: string, search: LogSearch) {
  const { deploymentId, projectSlug, environmentSlug } = search;
  const missing = new NotFound({ message: deploymentId ? "Deployment was not found." : "Environment was not found." });
  const query: ConfigQuery | null = deploymentId ? { query: "deployment", id: deploymentId }
    : projectSlug && environmentSlug ? { query: "namespace", environment: { project: projectSlug, environment: environmentSlug } }
    : null;
  if (query === null) return yield* new Validation({ message: "An environment is required." });
  const view = yield* readStore(organizationId, query).pipe(Effect.mapError(() => missing));
  if (view.view !== "deployment" && view.view !== "namespace") return yield* missing;
  return { namespace: view.namespace, serviceId: search.serviceId, deploymentId } satisfies LogFilter;
});

/** The response owns this scope until its consumer disconnects. */
export const openContainerLogs = Effect.fn("Runtime.openContainerLogs")(function* (request: Request, search: LogSearch) {
  const { organizationId } = yield* authorizeRuntimeOrganization({ headers: request.headers, organizationSlug: search.organizationSlug });
  const filter = yield* resolveLogFilter(organizationId, search);
  const scope = yield* Scope.make();
  const close = Scope.close(scope, Exit.void);
  const runtime = yield* OrganizationRuntime;
  const session = yield* runtime.open(organizationId).pipe(Effect.provideService(Scope.Scope, scope), Effect.onError(() => close));
  if (session.status !== "connected") {
    yield* close;
    if (search.before === undefined) return { type: "offline" as const };
    return yield* new Validation({ message: "Container logs are unavailable while the server is disconnected." });
  }
  if (search.before !== undefined) {
    return yield* session.connected.logHistory({ filter, before: search.before, limit: 200, signal: request.signal })
      .pipe(Effect.map(page => ({ type: "history" as const, page })), Effect.ensuring(close));
  }
  const options = { filter, tail: 200, signal: request.signal };
  // Both reads start only when the response pulls them.
  const tail = yield* session.connected.logs({ ...options, follow: false }).pipe(Effect.onError(() => close));
  const follow = yield* session.connected.logs({ ...options, follow: true }).pipe(Effect.onError(() => close));
  const events = backfillThenFollow(tail, follow);
  return { type: "stream" as const, events, close };
});
