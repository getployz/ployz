import "@tanstack/react-start/server-only";
import type { OwnedNamespace } from "@ployz/sdk";
import { sql } from "drizzle-orm";
import { Effect } from "effect";
import { readStore } from "#/modules/config-store/config-store.server";
import { StoreRefused } from "#/modules/config-store/store.contract";
import { Database } from "#/server/database.server";
import { Conflict } from "#/server/public-error";

/** Every Namespace the Organization owns, with the Project and Environment it belongs to. */
export const owners = (organizationId: string) =>
  readStore(organizationId, { query: "namespaces" }).pipe(Effect.map(({ namespaces }) => namespaces));

/**
 * The Store refused to read an Environment back: its rows were written by a newer Cloud, or damaged. Matches
 * `error::corrupt` in ployz-store by its prefix. Whatever needs every
 * Environment's Volumes still refuses, naming the Environment and the way out.
 */
export const unreadable = <E>(error: E) =>
  error instanceof StoreRefused && error.code === "internal" && error.message.startsWith("The Config Store holds an unreadable");

export const unreadableEnvironment = ({ project, environment }: { readonly project: string; readonly environment: string }) => new Conflict({
  userFacing: true,
  message: `Ployz Cloud can't read ${project}/${environment} from the Config Store, so it can't tell where that Environment's Volumes are. If Ployz Cloud was rolled back below the release that last wrote it, roll it forward, then try again; otherwise contact support.`,
});

/**
 * The Namespace Environment `environmentId` owns, or null before its first Deploy. Read by ID, so no other Environment
 * is read on the way: one the Store can't read fails only its own lookups.
 */
export const ownerOf = Effect.fn("ConfigStore.ownerOf")(function* (organizationId: string, environmentId: string) {
  const { drizzle } = yield* Database;
  const [owned] = yield* drizzle.execute<OwnedNamespace>(sql`
    select n.namespace, p.name as project, e.name as environment
    from config_namespace n
      join config_environment e on e.id = n.environment_id
      join config_project p on p.id = e.project_id
    where n.organization_id = ${organizationId} and n.environment_id = ${environmentId}`, "objects");
  return owned ?? null;
});

/**
 * The Namespace and runtime name of a Service in an Environment, with where it lives as `project/environment`, or null
 * when the Environment has no such Service.
 */
export const findService = Effect.fn("ConfigStore.findService")(function* (organizationId: string, environmentId: string, service: string) {
  const owned = yield* ownerOf(organizationId, environmentId);
  if (owned === null) return null;
  const view = yield* readStore(organizationId, { query: "namespace", environment: { project: owned.project, environment: owned.environment } });
  // A rename between the two reads can put another Environment under these names.
  if (view.environment.id !== environmentId) return null;
  const runtime = view.services[service];
  return runtime === undefined ? null : { namespace: owned.namespace, runtime, project: owned.project, environment: owned.environment, place: `${owned.project}/${owned.environment}` };
});

/**
 * Every variable name the Store ever held Sealed in the Environment that owns `namespace`, plus every template, for
 * scrubbing its Containers' logs. Null when the Organization owns no such Namespace, so the reader scrubs every value.
 */
export const sealedNames = Effect.fn("ConfigStore.sealedNames")(function* (organizationId: string, namespace: string) {
  const owned = (yield* owners(organizationId)).find((row) => row.namespace === namespace);
  if (owned === undefined) return null;
  const { names } = yield* readStore(organizationId, { query: "sealed_names", environment: { project: owned.project, environment: owned.environment } });
  return names;
});

export const environmentProtected = Effect.fn("ConfigStore.environmentProtected")(function* (organizationId: string, project: string, environmentId: string) {
  const { environments } = yield* readStore(organizationId, { query: "environments", project });
  return environments.find(({ id }) => id === environmentId)?.protected === true;
});
