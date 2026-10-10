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
  /** Read the Log Store instead of following: its newest page, or the one behind `cursor`. */
  history: Schema.optional(Schema.Literal("1")),
  cursor: Schema.optional(Schema.String.check(Schema.isMaxLength(4096))),
});
export type LogSearch = typeof logSearchSchema.Type;

/**
 * Whose logs: a Deployment's (its Namespace, then the containers labelled with its ID), or an Environment's (its
 * Namespace). The Store answers for the Organization only, so another's stays not found.
 */
export const resolveLogFilter = Effect.fn("Runtime.resolveLogFilter")(function* (organizationId: string, search: LogSearch) {
  const { deploymentId, projectSlug, environmentSlug } = search;
  const missing = new NotFound({ message: deploymentId ? "Deployment was not found." : "Environment was not found." });
  const query: Extract<ConfigQuery, { query: "deployment" | "namespace" }> | null = deploymentId ? { query: "deployment", id: deploymentId }
    : projectSlug && environmentSlug ? { query: "namespace", environment: { project: projectSlug, environment: environmentSlug } }
    : null;
  if (query === null) return yield* new Validation({ message: "An environment is required." });
  const view = yield* readStore(organizationId, query).pipe(Effect.mapError(() => missing));
  // A Deployment names its Services' runtime names, so the Log Store finds one even after it's removed.
  const node = "runtime_names" in view ? view.nodes.find(candidate => candidate.type === "service" && candidate.id === search.serviceId) : undefined;
  const serviceName = node && "runtime_names" in view ? view.runtime_names[node.name] ?? node.name : undefined;
  return { namespace: view.namespace, serviceId: search.serviceId, serviceName, deploymentId } satisfies LogFilter;
});

const HISTORY_PAGE = 500;

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
    if (search.history === undefined) return { type: "offline" as const };
    return yield* new Validation({ message: "Container logs are unavailable while the server is disconnected." });
  }
  if (search.history !== undefined) {
    return yield* session.connected.logHistory({ filter, cursor: search.cursor, limit: HISTORY_PAGE, signal: request.signal })
      .pipe(Effect.map(page => ({ type: "history" as const, page })), Effect.ensuring(close));
  }
  const options = { filter, tail: 200, signal: request.signal };
  // Both reads start only when the response pulls them.
  const tail = yield* session.connected.logs({ ...options, follow: false }).pipe(Effect.onError(() => close));
  const follow = yield* session.connected.logs({ ...options, follow: true }).pipe(Effect.onError(() => close));
  const events = backfillThenFollow(tail, follow);
  return { type: "stream" as const, events, close };
});
