import "@tanstack/react-start/server-only";
import { Effect, Option } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { readStore } from "#/modules/config-store/config-store.server";
import { requireInfrastructureOrganization } from "#/modules/runtime/organization-access.server";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import type { DataLossIdentity } from "#/modules/runtime/data-loss-identity";
import { Conflict } from "#/server/public-error";
import { SYSTEM_NAMESPACE } from "./server-services";

/** The Namespaces the Organization's Environments own, as the Store names them; one never deployed may have none. */
const ownedNamespaces = Effect.fn("NamespaceCleanup.owned")(function* (organizationId: string) {
  const { projects } = yield* readStore(organizationId, { query: "projects" });
  const environments = projects.flatMap((project) => project.environments.map((environment) => ({ project: project.name, environment })));
  const found = yield* Effect.forEach(environments, (environment) =>
    readStore(organizationId, { query: "namespace", environment }).pipe(Effect.option), { concurrency: 8 });
  return found.flatMap((view) => Option.isSome(view) ? [view.value.namespace] : []);
});

/** Which of `namespaces`, seen on the Organization's Servers, no Environment owns: the Servers page offers to remove them. */
export const listStrayNamespaces = Effect.fn("NamespaceCleanup.strays")(function* (
  actor: Actor, input: { organizationSlug: string; namespaces: readonly string[] },
) {
  const organization = yield* requireInfrastructureOrganization(actor, input.organizationSlug);
  const owned = new Set(yield* ownedNamespaces(organization.id));
  return input.namespaces.filter((namespace) => namespace !== SYSTEM_NAMESPACE && !owned.has(namespace));
});

/**
 * The Organization's Servers, for removing `namespace`: only one no Environment owns, and never Ployz's own. An owned
 * one goes with its Environment (Settings › Delete), which the Store records.
 */
const unownedSession = Effect.fn("NamespaceCleanup.session")(function* (actor: Actor, organizationSlug: string, namespace: string) {
  const organization = yield* requireInfrastructureOrganization(actor, organizationSlug);
  if (namespace === SYSTEM_NAMESPACE || (yield* ownedNamespaces(organization.id)).includes(namespace)) {
    return yield* new Conflict({ userFacing: true, message: `${namespace} belongs to an Environment: delete that Environment in its Settings instead.` });
  }
  const session = yield* (yield* OrganizationRuntime).open(organization.id);
  if (session.status !== "connected") return yield* new Conflict({ userFacing: true, message: "Your servers aren't answering. Try again once they are." });
  return session.connected;
});

/** What removing `namespace` from the Servers deletes: its Volumes' data, on each Server that holds it. */
export const loadNamespaceDataLoss = Effect.fn("NamespaceCleanup.dataLoss")(function* (
  actor: Actor, input: { organizationSlug: string; namespace: string },
) {
  const session = yield* unownedSession(actor, input.organizationSlug, input.namespace);
  const observed = yield* session.dataLossIfNamespaceDestroyed(input.namespace).pipe(
    Effect.mapError(() => new Conflict({ userFacing: true, message: "Couldn't check what this deletes on your servers. Try again." })));
  return observed.data_loss;
});

/**
 * Remove `namespace`, which no Environment owns, from every Server with its Volumes, deleting exactly `confirmDataLoss`.
 * Resolves to what the Servers hold beyond it when they hold more by now, to be confirmed again.
 */
export const removeNamespace = Effect.fn("NamespaceCleanup.remove")(function* (
  actor: Actor, input: { organizationSlug: string; namespace: string; confirmDataLoss: readonly DataLossIdentity[] },
) {
  const session = yield* unownedSession(actor, input.organizationSlug, input.namespace);
  const outcome = yield* session.destroyNamespace(input.namespace, { confirmed: [...input.confirmDataLoss] }).pipe(
    Effect.map((done) => done.type === "success" ? { removed: true as const } : { failed: "Some servers didn't finish removing it. Try again." }),
    Effect.catchTag("MissingDataLossIdentities", (missing) => Effect.succeed({ missing: missing.identities })),
    Effect.mapError(() => new Conflict({ userFacing: true, message: "Your servers didn't answer. Try again." })),
  );
  return outcome;
}, Effect.scoped);
