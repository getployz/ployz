import "@tanstack/react-start/server-only";
import type { ConfigCommand, ConfigCommitted, ConfigQuery, ConfigTrusted, ConfigWritten, PullRequestRef, SystemEvent } from "@ployz/sdk";
import { eq } from "drizzle-orm";
import { Effect, Option } from "effect";
import { gatherDomainEvidence, systemDomainEvidence } from "#/modules/config-store/domain-evidence.server";
import { gatherGitEvidence } from "#/modules/config-store/git-evidence.server";
import { gatherVolumeEvidence } from "#/modules/config-store/volume-evidence.server";
import type { Actor } from "#/modules/identity/actor";
import { user } from "#/modules/identity/tables";
import { cloudStore, storeTry } from "#/modules/config-store/store-sdk.server";
import { commandEnvironment, StoreRefused } from "#/modules/config-store/store.contract";
import { getOrganizationForUserBySlug } from "#/modules/organization/organization-state.server";
import { sendInngestEvent } from "#/modules/inngest/client";
import {
  createConfigDeploymentAdmittedEvent,
  createConfigDeploymentStartedEvent,
  createConfigPrCheckRequestedEvent,
  type ConfigDeploymentAdmittedEventData,
} from "#/modules/inngest/events";
import { Database } from "#/server/database.server";
import { NotFound } from "#/server/public-error";
import { cancelGithubRun } from "#/modules/github/github-build.server";
import { countOrganizationMachines, loadOrganizationConnections } from "#/modules/machines/connections.server";
import type { CommittedViews, StoreAnswer, StoreWriteResult, StoreCall, StoreRead, StoreRefusal, StoreResult } from "./store.contract";

/** One view Cloud reads for itself, with no evidence: its own lookups, such as a Deployment's Namespace for its logs. */
export const readStore = <Q extends ConfigQuery>(organizationId: string, query: Q) =>
  Effect.flatMap(cloudStore, (store) => storeTry(() => store.read(organizationId, query))).pipe(
    Effect.withSpan("ConfigStore.read"),
  );

/**
 * Stop a Deployment's builds still on GitHub: end their Build Grants, fail them, cancel their runs. Idempotent. Its
 * cancellation, and a worker that gave up on it, call it.
 */
export const cancelStoreGithubBuilds = Effect.fn("ConfigStore.cancelGithubBuilds")(function* (organizationId: string, deploymentId: string) {
  const store = yield* cloudStore;
  const connections = yield* connectionsOf(organizationId);
  const builds = yield* storeTry(() => store.githubCancel(deploymentId, connections));
  yield* Effect.forEach(builds, ({ run }) => cancelGithubRun({ installationId: run.installation_id, fullName: run.repository, runId: run.run_id }).pipe(
    Effect.ignore,
  ), { concurrency: 4, discard: true });
  return builds.length;
});

/**
 * Hand an admitted or started Deployment to Cloud's worker. A replayed admission sends the same event, which Inngest
 * drops, so a retried request starts one run; a start always sends. A failed send is logged: the admission stands,
 * and a minute later Cloud hands it over again (`createRedispatchStoreDeployments`).
 */
const dispatchAdmitted = Effect.fn("ConfigStore.dispatchAdmitted")(function* (
  organizationId: string,
  written: ConfigWritten,
  started: boolean,
) {
  for (const data of admittedEvents(organizationId, written)) {
    const event = started ? createConfigDeploymentStartedEvent(data) : createConfigDeploymentAdmittedEvent(data);
    yield* sendInngestEvent(event).pipe(
      Effect.catchTag("InngestEventSendError", (error) =>
        Effect.logWarning("An admitted Deployment waits to be handed over again: Cloud couldn't hand it to its worker.", { data, error })),
    );
  }
});

/**
 * The Deployments a write admitted, for Cloud's worker: one admitted or started, or those an automation admitted. A
 * removal with nothing on a Server applied at admission: no worker runs it.
 */
export function admittedEvents(organizationId: string, written: ConfigWritten): ConfigDeploymentAdmittedEventData[] {
  const summaries = written.written === "deployment" ? [written]
    : written.written === "automated" ? written.admitted.map((auto) => auto.deployment) : [];
  return summaries.filter((summary) => summary.status !== "applied")
    .map((summary) => ({ organizationId, environmentId: summary.environment_id, deploymentId: summary.id }));
}

/**
 * Ask for the check of each of `pulls` to be published again, after a write or a Deployment that may move it: the
 * Store names them. A failed request is logged: the pull request's next event publishes it anyway.
 */
export const requestChecks = Effect.fn("ConfigStore.requestChecks")(function* (organizationId: string, pulls: readonly PullRequestRef[]) {
  if (pulls.length === 0) return;
  yield* sendInngestEvent(pulls.map((pull) => createConfigPrCheckRequestedEvent({ organizationId, repositoryId: pull.repository_id, number: pull.number }))).pipe(
    Effect.catch((error) => Effect.logWarning("The PR checks were not requested.", { organizationId, error })),
  );
});

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

/** Who a user's write is by, as the Store records it (a Deployment's `admitted_by`): their name, else email. */
const principalFor = Effect.fn("ConfigStore.principalFor")(function* (userId: string | null) {
  if (userId === null) return null;
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ name: user.name, email: user.email }).from(user).where(eq(user.id, userId)).limit(1);
  return row === undefined ? null : row.name || row.email;
});

/** The Organization's Servers, to dial; none while it has no pairing. */
export const connectionsOf = Effect.fn("ConfigStore.connectionsOf")(function* (organizationId: string) {
  const loaded = yield* loadOrganizationConnections(organizationId);
  return loaded.kind === "ready" ? loaded.connections : [];
});

/** Something Cloud observed, told to the Organization's Store with how many Servers could run what it admits and the hostnames other Namespaces publish. */
export const storeSystem = Effect.fn("ConfigStore.system")(function* (organizationId: string, event: SystemEvent) {
  const store = yield* cloudStore;
  const servers = yield* countOrganizationMachines(organizationId);
  const domains = yield* systemDomainEvidence(organizationId);
  return yield* storeTry(() => store.system(organizationId, event, { servers, domains }));
});

/**
 * The trusted evidence `call` needs, as Cloud observes it itself: GitHub's for repository Services, what Cloud observes
 * of domains, for a Deploy that removes deployed Volumes what the Servers hold of them, and for an admission how many
 * Servers could run it. Nothing here comes from the caller. Refused `unavailable` when GitHub or the Cluster Domain
 * can't answer.
 */
export const gatherTrusted = Effect.fn("ConfigStore.gatherTrusted")(function* (
  organizationId: string, call: StoreCall, read: StoreRead,
) {
  const git = yield* gatherGitEvidence(organizationId, call, read).pipe(
    Effect.catchTag("GithubObservationError", () =>
      Effect.fail(new StoreRefused({ code: "unavailable", message: "GitHub didn't answer; retry.", details: null }))),
  );
  const domains = yield* gatherDomainEvidence(organizationId, call, read).pipe(
    Effect.catchTag("HostedDnsError", () => Effect.fail(new StoreRefused({
      code: "unavailable", message: "Cloud couldn't reserve the Cluster Domain; deploy again.", details: null,
    }))),
  );
  const volumes = yield* gatherVolumeEvidence(organizationId, call, read);
  // An admission carries how many Servers the Organization has enrolled, so the Store refuses one nothing could run.
  const servers = call.operation === "write" && call.command.command === "admit" ? yield* countOrganizationMachines(organizationId) : undefined;
  const trusted: ConfigTrusted = { ...git, domains };
  if (volumes !== undefined) trusted.volumes = volumes;
  if (servers !== undefined) trusted.servers = servers;
  return trusted;
});

/** What a committed write leaves Cloud to do: stop a cancelled Deployment's GitHub builds, run an admitted one, recheck PRs. */
const afterWrite = Effect.fn("ConfigStore.afterWrite")(function* (
  organizationId: string, command: ConfigCommand, written: ConfigCommitted,
) {
  if (command.command === "cancel") {
    // Cancellation ends outstanding Build Grants at once; best effort, as the walk's next look ends them too.
    yield* cancelStoreGithubBuilds(organizationId, command.deployment).pipe(
      Effect.catch((error) => Effect.logWarning("Could not stop a cancelled Deployment's GitHub builds.", error)),
    );
  }
  // An admitted (or retried) or started Deployment goes to Cloud's worker, whoever asked.
  if (command.command === "admit" || command.command === "start") {
    yield* dispatchAdmitted(organizationId, written, command.command === "start");
  }
  // A closing Branch whose removal applied at admission (nothing on a Server) goes now: no worker will sweep after it.
  // ponytail: what else this sweep leaves to do waits for the hourly one.
  if (command.command === "admit" && command.admit === "remove" && command.close === true
    && written.written === "deployment" && written.status === "applied") {
    yield* storeSystem(organizationId, { event: "sweep", now: Math.floor(Date.now() / 1000) });
  }
  yield* requestChecks(organizationId, written.checks);
});

/**
 * One Store read or write by user `userId` (null: Cloud itself) as `organizationId`: the answer, or the Store's refusal verbatim. It first
 * gathers the trusted evidence the call needs (`gatherTrusted`). Anything else (the Store failing to open, a broken
 * binding) is a defect.
 */
export const callStore = <C extends StoreCall>(organizationId: string, userId: string | null, call: C) => Effect.gen(function* () {
  const store = yield* cloudStore;
  const read: StoreRead = (query) => store.read(organizationId, query);
  return yield* Effect.gen(function* () {
    const trusted = yield* gatherTrusted(organizationId, call, read);
    if (call.operation === "read") return { ok: true, value: yield* storeTry(() => store.read(organizationId, call.query, trusted)) };
    const principal = yield* principalFor(userId).pipe(Effect.orDie);
    const written = yield* storeTry(() => store.write(organizationId, call.command, trusted, principal));
    // The write committed: nothing after it may answer as its refusal. What failed is logged; sweeps redo it.
    yield* afterWrite(organizationId, call.command, written).pipe(
      Effect.catchCause((cause) => Effect.logWarning("Cloud's follow-up to a committed Store write failed.", cause)),
    );
    return { ok: true, value: written };
  }).pipe(
    // SAFETY: a read answers its query's view, a write what it wrote.
    Effect.map((result) => result as StoreResult<StoreAnswer<C>>),
    Effect.catchTag("StoreRefused", (error) => Effect.succeed<StoreResult<never>>({ ok: false, refusal: error.refusal })),
  );
}).pipe(Effect.withSpan("ConfigStore.call"));

const memberOrganization = Effect.fn("ConfigStore.memberOrganization")(function* (actor: Actor, organizationSlug: string) {
  const organization = yield* getOrganizationForUserBySlug(actor.userId, organizationSlug).pipe(Effect.orDie);
  if (!organization) return yield* new NotFound({ message: "Organization not found." });
  return organization;
});

/**
 * The dashboard's way into the Store: one read or write as `actor`, in the Organization named by `organizationSlug`
 * when the actor is a member of it (the same Organization gate as the Org Store's reads).
 */
export const callStoreAsMember = <C extends StoreCall>(actor: Actor, organizationSlug: string, call: C) => Effect.gen(function* () {
  return yield* callStore((yield* memberOrganization(actor, organizationSlug)).id, actor.userId, call);
}).pipe(Effect.withSpan("ConfigStore.callAsMember"));

/**
 * The dashboard's write: `command` as `actor`, answered with the committed `diff`, `services` and Settings views of the
 * Environment it names, so the review's rows, count, pink and values arrive with the write. A view that can't be read is left
 * out (the writer's refetch brings it); a command naming no Environment, or several (a Move), carries none.
 */
export const writeStoreAsMember = (actor: Actor, organizationSlug: string, command: ConfigCommand) => Effect.gen(function* () {
  const organization = yield* memberOrganization(actor, organizationSlug);
  const result = yield* callStore(organization.id, actor.userId, { operation: "write", command });
  const environment = commandEnvironment(command);
  if (!result.ok || environment === null) return result satisfies StoreWriteResult;
  const views = yield* Effect.all({
    diff: readStore(organization.id, { query: "diff", environment }).pipe(Effect.option),
    services: readStore(organization.id, { query: "services", environment }).pipe(Effect.option),
    environment: readStore(organization.id, { query: "environment", environment, path: null, all: true }).pipe(Effect.option),
  }, { concurrency: 3 });
  const committed: CommittedViews = {};
  if (Option.isSome(views.diff)) committed.diff = views.diff.value;
  if (Option.isSome(views.services)) committed.services = views.services.value;
  if (Option.isSome(views.environment)) committed.environment = views.environment.value;
  return { ...result, views: committed } satisfies StoreWriteResult;
}).pipe(Effect.map((answer): StoreWriteResult => answer), Effect.withSpan("ConfigStore.writeAsMember"));
