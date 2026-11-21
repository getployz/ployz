import "@tanstack/react-start/server-only";
import { Effect, Exit, Schema, Scope } from "effect";
import type { ConfigQuery, LogFilter } from "@ployz/sdk";
import { authorizeRequest, requireAccessById } from "#/modules/organization/access.server";
import { type ConnectedRuntimeClient, OrganizationRuntime } from "./organization-runtime.server";
import { SealedNames } from "./ployz.server";
import { backfillThenFollow } from "./container-log-events.server";
import { sealedFeed } from "./sealed-names.server";
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
  /** The Engine's `namespace/name` for `serviceId`: lines in Cloud's history carry that name, not the Store's ID. */
  serviceIdentity: Schema.optional(Schema.String.check(Schema.isMaxLength(300))),
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
  // A Deployment names its Services' runtime names, so Cloud's history finds one even after it's removed.
  const node = "runtime_names" in view ? view.nodes.find(candidate => candidate.type === "service" && candidate.id === search.serviceId) : undefined;
  const prefix = `${view.namespace}/`;
  const named = search.serviceIdentity?.startsWith(prefix) ? search.serviceIdentity.slice(prefix.length) : undefined;
  const serviceName = node && "runtime_names" in view ? view.runtime_names[node.name] ?? node.name : named;
  if (search.serviceId !== undefined && serviceName === undefined) return yield* new NotFound({ message: "Service was not found." });
  return { namespace: view.namespace, serviceId: search.serviceId, serviceName, deploymentId } satisfies LogFilter;
});

/**
 * A selection's tail and follow, read from its Machines. History is Cloud's own (`readHistoryPage`). Every read scrubs
 * under one handle the Store's Sealed names feed while the scope lives, so one Store read serves them all.
 */
export const readContainerLogs = Effect.fn("Runtime.readContainerLogs")(function* (connected: ConnectedRuntimeClient, organizationId: string, filter: LogFilter & { namespace: string }, signal: AbortSignal, handle = new SealedNames()) {
  const sealed = yield* sealedFeed(organizationId, filter.namespace, handle);
  const options = { filter, sealed, tail: 200, signal };
  // Both reads start only when the response pulls them.
  const tail = yield* connected.logs({ ...options, follow: false });
  const follow = yield* connected.logs({ ...options, follow: true });
  return { events: backfillThenFollow(tail, follow) };
});

/** The response owns this scope until its consumer disconnects. */
export const openContainerLogs = Effect.fn("Runtime.openContainerLogs")(function* (request: Request, search: LogSearch) {
  const access = yield* authorizeRequest(request.headers, search.organizationSlug, "logs:read");
  if (access.kind === "support" && !access.shows.logs) return { type: "hidden" as const };
  const organizationId = access.organization.id;
  const filter = yield* resolveLogFilter(organizationId, search);
  const scope = yield* Scope.make();
  const close = Scope.close(scope, Exit.void);
  const runtime = yield* OrganizationRuntime;
  const session = yield* runtime.open(organizationId).pipe(Effect.provideService(Scope.Scope, scope), Effect.onError(() => close));
  if (session.status !== "connected") {
    yield* close;
    return { type: "offline" as const };
  }
  const read = yield* readContainerLogs(session.connected, organizationId, filter, request.signal)
    .pipe(Effect.provideService(Scope.Scope, scope), Effect.onError(() => close));
  // Support's access is read again while it streams, so hiding container logs stops lines already flowing.
  const logsShown = access.kind === "support"
    ? requireAccessById({ userId: access.userId }, organizationId, "logs:read").pipe(Effect.map(now => now.kind !== "support" || now.shows.logs))
    : undefined;
  return { type: "stream" as const, ...read, close, logsShown };
});
