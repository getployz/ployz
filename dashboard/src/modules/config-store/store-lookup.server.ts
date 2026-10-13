import "@tanstack/react-start/server-only";
import { Effect } from "effect";
import { readStore } from "#/modules/config-store/config-store.server";

/** Every Namespace the Organization owns, with the Project and Environment it belongs to. */
export const owners = (organizationId: string) =>
  readStore(organizationId, { query: "namespaces" }).pipe(Effect.map(({ namespaces }) => namespaces));

/** An Environment's Namespace and its deployed Services' runtime names, or null before its first Deployment. */
export const findNamespace = Effect.fn("ConfigStore.findNamespace")(function* (organizationId: string, environmentId: string) {
  for (const owned of yield* owners(organizationId)) {
    const view = yield* readStore(organizationId, { query: "namespace", environment: { project: owned.project, environment: owned.environment } });
    if (view.environment.id === environmentId) return { namespace: owned.namespace, services: view.services };
  }
  return null;
});

/** The Namespace and runtime name of a deployed Service in an Environment, or null when it has none. */
export const findService = Effect.fn("ConfigStore.findService")(function* (organizationId: string, environmentId: string, service: string) {
  const found = yield* findNamespace(organizationId, environmentId);
  const runtime = found?.services[service];
  return found === null || runtime === undefined ? null : { namespace: found.namespace, runtime };
});
