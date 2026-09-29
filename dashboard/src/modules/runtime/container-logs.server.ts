import "@tanstack/react-start/server-only";
import { and, eq } from "drizzle-orm";
import { Effect, Exit, Schema, Scope } from "effect";
import type { ConfigQuery, LogFilter } from "@ployz/sdk";
import { authorizeRuntimeOrganization } from "./authorize-runtime-organization.server";
import { OrganizationRuntime } from "./organization-runtime.server";
import { backfillThenFollow } from "./container-log-events.server";
import { Database } from "#/server/database.server";
import { NotFound, Validation } from "#/server/public-error";
import { environment } from "#/modules/project/tables";
import { environmentDeployment } from "#/modules/deployments/tables";
import { organizationSlugSchema } from "#/modules/organization/tables";
import { readStore } from "#/modules/config-store/config-store.server";

export const logSearchSchema = Schema.Struct({
  organizationSlug: organizationSlugSchema,
  environmentSlug: Schema.optional(Schema.String),
  /** A Config Store Environment is named within its Project. */
  projectSlug: Schema.optional(Schema.String),
  deploymentId: Schema.optional(Schema.String.check(Schema.isUUID())),
  serviceId: Schema.optional(Schema.String.check(Schema.isUUID())),
  before: Schema.optional(Schema.fromJsonString(Schema.Record(Schema.String, Schema.String.check(Schema.isPattern(/^-?\d{1,19}$/))))),
});
export type LogSearch = typeof logSearchSchema.Type;

export const resolveLogFilter = Effect.fn("Runtime.resolveLogFilter")(function* (organizationId: string, search: LogSearch) {
  const { drizzle } = yield* Database;
  let namespace = search.environmentSlug;
  if (search.deploymentId) {
    const [row] = yield* drizzle.select({ namespace: environment.namespace })
      .from(environmentDeployment).innerJoin(environment, eq(environment.id, environmentDeployment.environmentId))
      .where(and(eq(environmentDeployment.id, search.deploymentId), eq(environmentDeployment.organizationId, organizationId))).limit(1);
    if (!row) return yield* new NotFound({ message: "Deployment was not found." });
    namespace = row.namespace;

  } else {
    if (!namespace) return yield* new Validation({ message: "An environment is required." });
    const [row] = yield* drizzle.select({ id: environment.id }).from(environment)
      .where(and(eq(environment.namespace, namespace), eq(environment.organizationId, organizationId))).limit(1);
    if (!row) return yield* new NotFound({ message: "Environment was not found." });
  }
  return { namespace, serviceId: search.serviceId, deploymentId: search.deploymentId } satisfies LogFilter;
});

/**
 * A Config Store Deployment's logs (its Namespace, then the containers labelled with its ID), or a Config Store
 * Environment's (its Namespace). The Store answers for the Organization only, so another's stays not found.
 */
// TODO(#1275): the only filter, once the Store is the only backend.
const storeFilter = Effect.fn("Runtime.storeFilter")(function* (organizationId: string, search: LogSearch, missing: NotFound) {
  const { deploymentId, projectSlug, environmentSlug } = search;
  const query: ConfigQuery | null = deploymentId ? { query: "deployment", id: deploymentId }
    : projectSlug && environmentSlug ? { query: "namespace", environment: { project: projectSlug, environment: environmentSlug } }
    : null;
  if (query === null) return yield* missing;
  const view = yield* readStore(organizationId, query).pipe(Effect.mapError(() => missing));
  if (view.view !== "deployment" && view.view !== "namespace") return yield* missing;
  return { namespace: view.namespace, serviceId: search.serviceId, deploymentId } satisfies LogFilter;
});

/** The response owns this scope until its consumer disconnects. */
export const openContainerLogs = Effect.fn("Runtime.openContainerLogs")(function* (request: Request, search: LogSearch) {
  const { organizationId } = yield* authorizeRuntimeOrganization({ headers: request.headers, organizationSlug: search.organizationSlug });
  const filter = yield* resolveLogFilter(organizationId, search).pipe(
    Effect.catchTag("NotFound", (missing) => storeFilter(organizationId, search, missing)),
  );
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
