import "@tanstack/react-start/server-only";
import { Effect, Exit, Schema, Scope } from "effect";
import { type ConfigQuery, type LogFilter, SealedNames } from "@ployz/sdk";
import { authorizeRequest, requireAccessById } from "#/modules/organization/access.server";
import { type ConnectedRuntimeClient, OrganizationRuntime } from "./organization-runtime.server";
import { backfillThenFollow, withheld } from "./container-log-events.server";
import { sealedFeed } from "./sealed-names.server";
import { Forbidden, NotFound, Validation } from "#/server/public-error";
import { organizationSlugSchema } from "#/modules/organization/tables";
import { readStore } from "#/modules/config-store/config-store.server";

export const logSearchSchema = Schema.Struct({
  organizationSlug: organizationSlugSchema,
  environmentSlug: Schema.optional(Schema.String),
  /** An Environment is named within its Project. */
  projectSlug: Schema.optional(Schema.String),
  deploymentId: Schema.optional(Schema.String.check(Schema.isUUID())),
  serviceId: Schema.optional(Schema.String.check(Schema.isUUID())),
});
export type LogSearch = typeof logSearchSchema.Type;

/** A read of the Log Store instead of following: its newest page, or the one behind `cursor`. The cursor holds a place per Server, so it travels in a body, not a URL. */
export const logHistorySchema = Schema.Struct({
  ...logSearchSchema.fields,
  cursor: Schema.optional(Schema.String.check(Schema.isMaxLength(1 << 18))),
});

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

/**
 * A page of `filter`'s stored logs, or its tail and follow. Every read scrubs under one handle the Store's Sealed names
 * feed while the scope lives, so one Store read serves them all; a Machine's failure reaches the viewer as fixed copy.
 */
export const readContainerLogs = Effect.fn("Runtime.readContainerLogs")(function* (connected: ConnectedRuntimeClient, organizationId: string, filter: LogFilter & { namespace: string }, signal: AbortSignal, history?: { cursor?: string }, handle = new SealedNames()) {
  const sealed = yield* sealedFeed(organizationId, filter.namespace, handle);
  if (history !== undefined) {
    const page = yield* connected.logHistory({ filter, sealed, cursor: history.cursor, limit: HISTORY_PAGE, signal });
    return { type: "history" as const, page: { ...page, failures: page.failures.map(withheld) } };
  }
  const options = { filter, sealed, tail: 200, signal };
  // Both reads start only when the response pulls them.
  const tail = yield* connected.logs({ ...options, follow: false });
  const follow = yield* connected.logs({ ...options, follow: true });
  return { type: "stream" as const, events: backfillThenFollow(tail, follow) };
});

/** The response owns this scope until its consumer disconnects. */
export const openContainerLogs = Effect.fn("Runtime.openContainerLogs")(function* (request: Request, search: LogSearch, history?: { cursor?: string }) {
  const access = yield* authorizeRequest(request.headers, search.organizationSlug, "logs:read");
  if (access.kind === "support" && !access.shows.logs) {
    if (history === undefined) return { type: "hidden" as const };
    return yield* new Forbidden({ message: "This Organization doesn't show its container logs to Ployz support." });
  }
  const organizationId = access.organization.id;
  const filter = yield* resolveLogFilter(organizationId, search);
  const scope = yield* Scope.make();
  const close = Scope.close(scope, Exit.void);
  const runtime = yield* OrganizationRuntime;
  const session = yield* runtime.open(organizationId).pipe(Effect.provideService(Scope.Scope, scope), Effect.onError(() => close));
  if (session.status !== "connected") {
    yield* close;
    if (history === undefined) return { type: "offline" as const };
    return yield* new Validation({ message: "Container logs are unavailable while the server is disconnected." });
  }
  const read = yield* readContainerLogs(session.connected, organizationId, filter, request.signal, history)
    .pipe(Effect.provideService(Scope.Scope, scope), Effect.onError(() => close));
  if (read.type === "history") {
    yield* close;
    return read;
  }
  // Support's access is read again while it streams, so hiding container logs stops lines already flowing.
  const logsShown = access.kind === "support"
    ? requireAccessById({ userId: access.userId }, organizationId, "logs:read").pipe(Effect.map(now => now.kind !== "support" || now.shows.logs))
    : undefined;
  return { ...read, close, logsShown };
});
