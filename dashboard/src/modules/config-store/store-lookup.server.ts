import "@tanstack/react-start/server-only";
import { Effect } from "effect";
import { readStore } from "#/modules/config-store/config-store.server";

/** Every Namespace the Organization owns, with the Project and Environment it belongs to. */
export const owners = (organizationId: string) =>
  readStore(organizationId, { query: "namespaces" }).pipe(Effect.map(({ namespaces }) => namespaces));

/** The Namespace and runtime name of a Service in an Environment, or null when the Environment has no such Service. */
export const findService = Effect.fn("ConfigStore.findService")(function* (organizationId: string, environmentId: string, service: string) {
  for (const owned of yield* owners(organizationId)) {
    const view = yield* readStore(organizationId, { query: "namespace", environment: { project: owned.project, environment: owned.environment } });
    if (view.environment.id !== environmentId) continue;
    const runtime = view.services[service];
    return runtime === undefined ? null : { namespace: owned.namespace, runtime };
  }
  return null;
});
