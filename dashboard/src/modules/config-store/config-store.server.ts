import "@tanstack/react-start/server-only";
import { createRequire } from "node:module";
import type * as PloyzSdk from "@ployz/sdk";
import type { ConfigQuery, ConfigStore, ConfigTrusted, ConfigView, ConfigWritten } from "@ployz/sdk";
import { eq, sql } from "drizzle-orm";
import { Data, Effect, Option, Redacted, Schema } from "effect";
import { gatherDomainEvidence } from "#/modules/config-store/domain-evidence.server";
import { GitCommand, gatherGitEvidence } from "#/modules/config-store/git-evidence.server";
import { AdmitCommand, gatherVolumeEvidence } from "#/modules/config-store/volume-evidence.server";
import type { Actor } from "#/modules/identity/actor";
import { user } from "#/modules/identity/tables";
import { storeTry } from "#/modules/config-store/store-sdk.server";
import { getOrganizationForUserBySlug } from "#/modules/environment-design/workspace-repository.server";
import { sendInngestEvent } from "#/modules/inngest/client";
import {
  createConfigDeploymentAdmittedEvent,
  createConfigDeploymentStartedEvent,
  createConfigPrCheckRequestedEvent,
} from "#/modules/inngest/events";
import { storeChangeSources } from "#/modules/organization/change-log.sources";
import { AppConfig } from "#/server/config.server";
import { Database, type DatabaseService } from "#/server/database.server";
import { NotFound } from "#/server/public-error";
import { cancelGithubRun } from "#/modules/github/github-build.server";
import { loadOrganizationConnections } from "#/modules/machines/connections.server";
import type { StoreCall, StoreRefusal, StoreResult } from "./store.contract";

// SAFETY: the package exports this named CommonJS SDK surface at runtime.
const { openConfigStore, RpcError } = createRequire(import.meta.url)("@ployz/sdk") as Pick<
  typeof PloyzSdk,
  "openConfigStore" | "RpcError"
>;

/**
 * The Store creates its tables when it first opens, after Cloud's migrations have run, so Cloud attaches its
 * change log to them then. The lock serializes processes opening the Store at once.
 */
const attachChangeLog = Effect.gen(function* () {
  const database = yield* Database;
  yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* drizzle.execute(sql`select pg_advisory_xact_lock(1256)`);
    for (const [table, { key }] of Object.entries(storeChangeSources)) {
      const keys = sql.join(key.map((column) => sql`${column}::text`), sql`, `);
      yield* drizzle.execute(sql`
        select organization_change_attach(${table}::regclass, 'organization_id', variadic array[${keys}])
        where not exists (select from pg_trigger where tgrelid = ${table}::regclass and tgname = 'organization_change_insert')
      `);
    }
  }));
});

/** One Store handle per database for the process; a failed open is retried by the next call. */
const opened = new Map<string, Promise<ConfigStore>>();

/**
 * The Config Store in `database`, whose URL is `url`, with Cloud's change log attached to its tables. It seals secrets
 * with Cloud's encryption secret, so its ciphertext stays readable by Cloud's own sealing.
 */
export function storeAt(url: string, database: DatabaseService, sealingSecret: string) {
  let store = opened.get(url);
  if (store === undefined) {
    store = openConfigStore(url, sealingSecret).then(async (handle) => {
      await Effect.runPromise(attachChangeLog.pipe(Effect.provideService(Database, database)));
      return handle;
    });
    opened.set(url, store);
    store.catch(() => opened.delete(url));
  }
  return store;
}

export class ConfigStoreOpenFailure extends Data.TaggedError("ConfigStoreOpenFailure")<{ readonly cause: unknown }> {
  readonly publicErrorCategory = "internal" as const;
}

/** Cloud's Config Store, in Cloud's database. */
export const cloudStore = Effect.gen(function* () {
  const config = yield* AppConfig;
  const database = yield* Database;
  return yield* storeTry(() => storeAt(config.database.url.href, database, Redacted.value(config.encryptionSecret))).pipe(
    Effect.mapError((cause) => new ConfigStoreOpenFailure({ cause })),
  );
});

/** One view Cloud reads for itself, with no evidence: its own lookups, such as a Deployment's Namespace for its logs. */
export const readStore = Effect.fn("ConfigStore.read")(function* (organizationId: string, query: ConfigQuery) {
  const store = yield* cloudStore;
  // A refusal (`not_found`) is the RpcError itself.
  return yield* storeTry(() => store.read(organizationId, query));
});

/**
 * Stop a Deployment's builds still on GitHub: end their Build Grants, fail them, cancel their runs. Idempotent. Its
 * cancellation, and a worker that gave up on it, call it.
 */
export const cancelStoreGithubBuilds = Effect.fn("ConfigStore.cancelGithubBuilds")(function* (organizationId: string, deploymentId: string) {
  const store = yield* cloudStore;
  const loaded = yield* loadOrganizationConnections(organizationId);
  const connections = loaded.kind === "ready" ? loaded.connections : [];
  const builds = yield* storeTry(() => store.githubCancel(deploymentId, connections)).pipe(
    Effect.mapError((cause) => new ConfigStoreOpenFailure({ cause })),
  );
  yield* Effect.forEach(builds, ({ run }) => cancelGithubRun({ installationId: run.installation_id, fullName: run.repository, runId: run.run_id }).pipe(
    Effect.ignore,
  ), { concurrency: 4, discard: true });
  return builds.length;
});

/**
 * Hand an admitted or started Deployment to Cloud's worker; the refusal to answer instead when it can't. A replayed
 * admission sends the same event, which Inngest drops, so a retried request starts one run. A start always sends.
 */
const dispatchAdmitted = Effect.fn("ConfigStore.dispatchAdmitted")(function* (
  organizationId: string,
  written: ConfigWritten,
  read: (query: ConfigQuery) => Promise<ConfigView>,
  started: boolean,
) {
  if (written.written !== "deployment") return undefined;
  const view = yield* storeTry(() => read({ query: "deployment", id: written.id }));
  if (view.view !== "deployment") return yield* Effect.die(new Error("A deployment query answered another view"));
  const data = { organizationId, environmentId: view.environment.id, deploymentId: written.id };
  const event = started ? createConfigDeploymentStartedEvent(data) : createConfigDeploymentAdmittedEvent(data);
  return yield* sendInngestEvent(event).pipe(
    Effect.as(undefined),
    Effect.catchTag("InngestEventSendError", () => Effect.succeed<StoreRefusal>({
      code: "unavailable", message: "Cloud couldn't start this Deployment; deploy again.", details: null,
    })),
  );
});

/**
 * Ask for each pull request a Move named (a Conditional Save, for one) to have its check published again. A failed
 * request is logged: the write stands, and the pull request's next event publishes it anyway.
 */
const requestChecks = Effect.fn("ConfigStore.requestChecks")(function* (organizationId: string, written: ConfigWritten) {
  if (written.written !== "moved") return;
  for (const check of written.checks) {
    yield* sendInngestEvent(createConfigPrCheckRequestedEvent({ organizationId, repositoryId: check.repository_id, number: check.number })).pipe(
      Effect.catch((error) => Effect.logWarning("The PR check was not requested.", { organizationId, check, error })),
    );
  }
});

/** The Store's refusal in `cause`, if it is one. */
export const storeRefusal = (cause: unknown): StoreRefusal | null =>
  cause instanceof RpcError ? { code: cause.code, message: cause.message, details: cause.details } : null;

/** A Store refusal travels to the CLI verbatim: the RPC error vocabulary, never the rejected value. */
function statusFor(code: string) {
  switch (code) {
    case "invalid_argument":
      return 422;
    case "not_found":
      return 404;
    case "conflict":
    case "ambiguous":
    case "confirmation_required":
      return 409;
    case "unauthenticated":
      return 401;
    case "unsupported":
      return 501;
    case "unavailable":
      return 503;
    default:
      return 500;
  }
}

export function refusal(error: StoreRefusal) {
  const { code, message, details } = error;
  return Response.json({ error: { code, message, details } }, {
    status: statusFor(code),
    headers: { "cache-control": "no-store" },
  });
}

/** Who admitted an upload, as the Deployment's provenance names them: the signed-in user's name, else email. */
const uploaderFor = Effect.fn("ConfigStore.uploaderFor")(function* (call: StoreCall, userId: string) {
  if (call.operation !== "write" || call.command.command !== "admit" || !call.command.upload) return undefined;
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ name: user.name, email: user.email }).from(user).where(eq(user.id, userId)).limit(1);
  return row === undefined ? undefined : row.name || row.email;
});

/**
 * One Store read or write by user `userId` as `organizationId`: the answer, or the Store's refusal verbatim. It first
 * gathers the trusted evidence the call needs: GitHub's for repository Services, what Cloud observes of domains, for a
 * Deploy that removes deployed Volumes, what the Servers hold of them, and who admits an upload. Anything else (the
 * Store failing to open, a broken binding) is a defect.
 */
export const callStore = Effect.fn("ConfigStore.call")(function* (organizationId: string, userId: string, call: StoreCall) {
  const store = yield* cloudStore;
  const read = (query: ConfigQuery) => store.read(organizationId, query);
  const git = call.operation === "write"
    ? yield* gatherGitEvidence(organizationId, Option.getOrUndefined(Schema.decodeUnknownOption(GitCommand)(call.command)), read).pipe(
      Effect.catchTag("GithubObservationError", () => Effect.succeed(null)),
    )
    : { repositories: [] };
  if (git === null) {
    return { ok: false, refusal: { code: "unavailable", message: "GitHub didn't answer; retry.", details: null } } satisfies StoreResult<never>;
  }
  const domains = yield* gatherDomainEvidence(organizationId, call, read).pipe(
    Effect.catchTag("HostedDnsError", () => Effect.succeed(null)),
  );
  if (domains === null) {
    const refusal = { code: "unavailable", message: "Cloud couldn't reserve the Cluster Domain; deploy again.", details: null };
    return { ok: false, refusal } satisfies StoreResult<never>;
  }
  const volumes = call.operation === "write"
    ? yield* gatherVolumeEvidence(organizationId, Option.getOrUndefined(Schema.decodeUnknownOption(AdmitCommand)(call.command)), read)
    : undefined;
  const uploader = yield* uploaderFor(call, userId).pipe(Effect.orDie);
  const trusted: ConfigTrusted = { ...git, domains, uploader: uploader ?? null };
  if (volumes !== undefined) trusted.volumes = volumes;
  return yield* storeTry(async (): Promise<StoreResult<ConfigView | ConfigWritten>> => {
    const value = call.operation === "read"
      ? await store.read(organizationId, call.query, trusted)
      : await store.write(organizationId, call.command, trusted);
    return { ok: true, value };
  }).pipe(
    // An admitted (or retried) or started Deployment goes to Cloud's worker, whoever asked.
    Effect.flatMap((result) => Effect.gen(function* () {
      if (!result.ok || call.operation !== "write") return result;
      const command = call.command;
      if (command.command === "cancel") {
        // Cancellation ends outstanding Build Grants at once; best effort, as the walk's next look ends them too.
        yield* cancelStoreGithubBuilds(organizationId, command.deployment).pipe(
          Effect.catch((error) => Effect.logWarning("Could not stop a cancelled Deployment's GitHub builds.", error)),
        );
        return result;
      }
      // SAFETY: a write answers what it wrote.
      const written = result.value as ConfigWritten;
      if (command.command === "move") {
        yield* requestChecks(organizationId, written);
        return result;
      }
      if (command.command !== "admit" && command.command !== "start") return result;
      const refused = yield* dispatchAdmitted(organizationId, written, read, command.command === "start");
      return refused === undefined ? result : { ok: false, refusal: refused } satisfies StoreResult<never>;
    })),
    Effect.catch((cause) => cause instanceof RpcError
      ? Effect.succeed<StoreResult<never>>({ ok: false, refusal: { code: cause.code, message: cause.message, details: cause.details } })
      : Effect.die(cause)),
  );
});

/**
 * The dashboard's way into the Store: one read or write as `actor`, in the Organization named by `organizationSlug`
 * when the actor is a member of it (the same Organization gate as the Org Store's reads).
 */
export const callStoreAsMember = Effect.fn("ConfigStore.callAsMember")(function* (
  actor: Actor, organizationSlug: string, call: StoreCall,
) {
  const config = yield* AppConfig;
  // TODO(#1275): dark in production until the Config Store cutover.
  if (config.nodeEnv === "production") return yield* new NotFound({ message: "Not found." });
  const organization = yield* getOrganizationForUserBySlug(actor.userId, organizationSlug).pipe(Effect.orDie);
  if (!organization) return yield* new NotFound({ message: "Organization not found." });
  // Only Cloud's own Organization removal forgets an Organization's configuration.
  if (call.operation === "write" && call.command.command === "remove_organization") {
    return { ok: false, refusal: { code: "unsupported", message: "Delete an Organization from its settings.", details: null } } satisfies StoreResult<never>;
  }
  return yield* callStore(organization.id, actor.userId, call);
});
