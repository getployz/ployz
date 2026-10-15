import "@tanstack/react-start/server-only";
import type { Approval, ConfigCommand, GithubBuild, ConfigCommitted, ConfigQuery, ConfigTrusted, ConfigView, ConfigWritten, DeploymentSummary, PullRequestRef, SystemEvent } from "@ployz/sdk";
import { eq } from "drizzle-orm";
import { Effect, Option } from "effect";
import { gatherDomainEvidence, systemDomainEvidence } from "#/modules/config-store/domain-evidence.server";
import { gatherGitEvidence } from "#/modules/config-store/git-evidence.server";
import { gatherVolumeEvidence, systemVolumeEvidence } from "#/modules/config-store/volume-evidence.server";
import type { Actor } from "#/modules/identity/actor";
import { user } from "#/modules/identity/tables";
import { cloudStore, refusedWith, storeTry } from "#/modules/config-store/store-sdk.server";
import { commandEnvironment, StoreRefused } from "#/modules/config-store/store.contract";
import { deploymentRun } from "#/modules/config-store/tables";
import { requireAccess } from "#/modules/organization/access.server";
import { answerForSupport, viewForSupport } from "#/modules/support-access/support-view";
import { sendInngestEvent } from "#/modules/inngest/client";
import {
  createConfigDeploymentAdmittedEvent,
  createConfigDeploymentStartedEvent,
  createConfigPrCheckRequestedEvent,
  createConfigSweepRequestedEvent,
  createGithubBuildRunCompletedEvent,
  type ConfigDeploymentAdmittedEventData,
  type DeploymentTrigger,
} from "#/modules/inngest/events";
import { Database } from "#/server/database.server";
import { PostHog } from "#/modules/analytics/posthog.server";
import { installationAccess } from "#/modules/github/github-access";
import { cancelGithubRun } from "#/modules/github/github-build.server";
import { countOrganizationMachines, loadOrganizationConnections } from "#/modules/machines/connections.server";
import { writeAdmitted } from "#/modules/volume-placement/volume-placement.server";
import type { CommittedViews, StoreAnswer, StoreWriteResult, StoreCall, StoreRead, StoreRefusal, StoreResult } from "./store.contract";

/** One view Cloud reads for itself, with no evidence: its own lookups, such as a Deployment's Namespace for its logs. */
export const readStore = <Q extends ConfigQuery>(organizationId: string, query: Q) =>
  Effect.flatMap(cloudStore, (store) => storeTry(() => store.read(organizationId, query))).pipe(
    Effect.withSpan("ConfigStore.read"),
  );

/**
 * Cancel a run Cloud dispatched, as Cloud itself through the installation that runs it: the Organization's own token
 * is refused once it disconnected. A run that already ended is fine. GitHub refuses every token of an installation it
 * suspended or uninstalled, so those runs end at the runner's next refused call to Cloud instead.
 */
export const cancelDispatchedGithubRun = (run: GithubBuild["run"]) =>
  cancelGithubRun({ access: installationAccess(run.installation_id), fullName: run.repository, runId: run.run_id }).pipe(
    Effect.catch((error) => Effect.logWarning("Could not cancel a GitHub run Cloud dispatched.", { runId: run.run_id, error })),
  );

/**
 * Stop a Deployment's builds still on GitHub: fail them and cancel their runs. Their Build Grants expire by
 * themselves. Idempotent. Its cancellation, and a worker that gave up on it, call it.
 */
export const cancelStoreGithubBuilds = Effect.fn("ConfigStore.cancelGithubBuilds")(function* (deploymentId: string) {
  const store = yield* cloudStore;
  const builds = yield* storeTry(() => store.githubCancel(deploymentId));
  yield* Effect.forEach(builds, (build) => cancelDispatchedGithubRun(build.run), { concurrency: 4, discard: true });
  return builds.length;
});

/**
 * Stop the GitHub builds that `organizationIds` run through `installationId` once they can't use it: disconnected,
 * suspended or uninstalled. Call it after that change is recorded. Each run is cancelled and its walk woken, so the
 * walk looks now rather than at its next interval, finds the change, and fails the Deployment with it. Idempotent: an
 * ended run answers 409, and the walk ignores a wake for a build it already failed.
 */
export const stopGithubInstallationBuilds = Effect.fn("ConfigStore.stopGithubInstallationBuilds")(function* (
  installationId: number, organizationIds: ReadonlyArray<string>,
) {
  const store = yield* cloudStore;
  const { drizzle } = yield* Database;
  const stopped: string[] = [];
  for (const organizationId of organizationIds) {
    const runs = yield* drizzle.select({ deploymentId: deploymentRun.deploymentId }).from(deploymentRun)
      .where(eq(deploymentRun.organizationId, organizationId));
    for (const { deploymentId } of runs) {
      const view = yield* storeTry(() => store.read(organizationId, { query: "deployment", id: deploymentId })).pipe(
        Effect.catchIf(refusedWith("not_found"), () => Effect.succeed(null)));
      for (const build of view?.builds ?? []) {
        if (build.status !== "building") continue;
        const github = yield* storeTry(() => store.githubBuild(`${deploymentId}.${build.service}`)).pipe(
          Effect.catchIf(refusedWith("not_found"), () => Effect.succeed(null)));
        if (github === null || github.status !== "building" || github.run.installation_id !== installationId) continue;
        yield* cancelDispatchedGithubRun(github.run);
        yield* sendInngestEvent(createGithubBuildRunCompletedEvent({ id: `stopped-${github.id}`, runId: github.run.run_id }));
        stopped.push(github.id);
      }
    }
  }
  return stopped;
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
  for (const data of admittedEvents(organizationId, written, "user")) {
    const event = started ? createConfigDeploymentStartedEvent(data) : createConfigDeploymentAdmittedEvent(data);
    yield* sendInngestEvent(event).pipe(
      Effect.catchTag("InngestEventSendError", (error) =>
        Effect.logWarning("An admitted Deployment waits to be handed over again: Cloud couldn't hand it to its worker.", { data, error })),
    );
  }
});

/** The Deployments a write admitted: one admitted or started, those an automation admitted, or a removal's first ones. */
function admittedBy(written: ConfigWritten): DeploymentSummary[] {
  switch (written.written) {
    case "deployment": return [written];
    case "automated": return written.admitted.map((auto) => auto.deployment);
    case "removal": return written.removal === "removing" ? written.left.flatMap((left) => left.deployment ?? []) : [];
    default: return [];
  }
}

/**
 * The Deployments a write admitted, for Cloud's worker. A removal with nothing on a Server applied at admission: no
 * worker runs it.
 */
export function admittedEvents(organizationId: string, written: ConfigWritten, trigger: DeploymentTrigger): ConfigDeploymentAdmittedEventData[] {
  return admittedBy(written).filter((summary) => summary.status !== "applied")
    .map((summary) => ({ organizationId, environmentId: summary.environment_id, deploymentId: summary.id, trigger }));
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
    case "invalid_input":
      return 422;
    case "not_found":
    case "unknown_command":
      return 404;
    case "conflict":
    case "ambiguous":
    case "confirmation_required":
    case "approval_required":
    case "channel_mismatch":
      return 409;
    case "unauthenticated":
      return 401;
    case "forbidden":
    case "approval_denied":
      return 403;
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

/** Which events can admit a Deployment, and so need to know which Servers hold the data a Save accepted losing. */
const deploys = {
  branch_head: true, check_suite: true, pull_request: true, sweep: false, cluster_forgotten: false,
} satisfies Record<SystemEvent["event"], boolean>;

/**
 * Something Cloud observed, told to the Organization's Store with how many Servers could run what it admits, the
 * hostnames other Namespaces publish, and, for an event that can deploy, which Servers hold the data a Save accepted losing.
 */
export const storeSystem = Effect.fn("ConfigStore.system")(function* (organizationId: string, event: SystemEvent) {
  const store = yield* cloudStore;
  const servers = yield* countOrganizationMachines(organizationId);
  const domains = yield* systemDomainEvidence(organizationId);
  const volumes = deploys[event.event]
    ? yield* systemVolumeEvidence(organizationId, (query) => store.read(organizationId, query))
    : undefined;
  const trusted: Pick<ConfigTrusted, "servers" | "domains" | "volumes"> = { servers, domains };
  if (volumes !== undefined) trusted.volumes = volumes;
  return yield* storeTry(() => store.system(organizationId, event, trusted));
});

/**
 * The trusted evidence `call` needs, as Cloud observes it itself: GitHub's for repository Services, what Cloud observes
 * of domains, for a Save or Deploy that removes deployed Volumes what the Servers hold of them, and for an admission how many
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
  const admits = call.operation === "write" && (call.command.command === "admit" || call.command.command === "remove");
  const servers = admits ? yield* countOrganizationMachines(organizationId) : undefined;
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
    yield* cancelStoreGithubBuilds(command.deployment).pipe(
      Effect.catch((error) => Effect.logWarning("Could not stop a cancelled Deployment's GitHub builds.", error)),
    );
  }
  // An admitted (or retried) or started Deployment goes to Cloud's worker, whoever asked, as do a removal's first ones.
  if (command.command === "admit" || command.command === "start" || command.command === "remove") {
    yield* dispatchAdmitted(organizationId, written, command.command === "start");
  }
  // A removal that applied at admission (nothing on a Server) is deleted by the next sweep, and admits what is due
  // after it: no worker will ask for that sweep, so ask now.
  const removed = written.written === "deployment" ? written.remove && written.status === "applied"
    : written.written === "removal" && written.removal === "removing" && written.left.some((left) => left.deployment?.status === "applied");
  if (removed) yield* sendInngestEvent(createConfigSweepRequestedEvent({ organizationId }));
  yield* requestChecks(organizationId, written.checks);
});

/** Where a member's Store write came from, for product analytics: the dashboard, the CLI and the coding agent it detected, or the dashboard's own agent. */
export type StoreSource =
  | { readonly source: "dashboard" }
  | { readonly source: "cli"; readonly agent: string | null }
  | { readonly source: "agent" };

/**
 * One Store read or write by user `userId` (null: Cloud itself) as `organizationId`: the answer, or the Store's refusal verbatim. It first
 * gathers the trusted evidence the call needs (`gatherTrusted`), and a Deploy meets the Volume copies it takes
 * (`writeAdmitted`) before the Store sees it; `approval` is what a human approved, which only a CLI
 * write in an Organization that asks before destructive actions carries. Anything else (the Store failing to open, a broken
 * binding) is a defect.
 */
export const callStore = <C extends StoreCall>(
  organizationId: string,
  userId: string | null,
  call: C,
  source: StoreSource = { source: "dashboard" },
  approval: Approval = "not_required",
) => Effect.gen(function* () {
  const store = yield* cloudStore;
  const read: StoreRead = (query) => store.read(organizationId, query);
  return yield* Effect.gen(function* () {
    const trusted = { ...yield* gatherTrusted(organizationId, call, read), approval };
    if (call.operation === "read") return { ok: true, value: yield* storeTry(() => store.read(organizationId, call.query, trusted)) };
    const principal = yield* principalFor(userId).pipe(Effect.orDie);
    const written = yield* writeAdmitted(organizationId, call.command, (command) =>
      storeTry(() => store.write(organizationId, command, trusted, principal)));
    // The write committed: nothing after it may answer as its refusal. What failed is logged; sweeps redo it.
    yield* afterWrite(organizationId, call.command, written).pipe(
      Effect.catchCause((cause) => Effect.logWarning("Cloud's follow-up to a committed Store write failed.", cause)),
    );
    // One event for every command, so dashboards follow the domain, not the UI. Cloud's own writes aren't a user's.
    if (userId !== null) {
      yield* (yield* PostHog).capture({
        userId,
        organizationId,
        event: "config_command",
        properties: { command: call.command.command, ...source },
      });
    }
    return { ok: true, value: written };
  }).pipe(
    // SAFETY: a read answers its query's view, a write what it wrote.
    Effect.map((result) => result as StoreResult<StoreAnswer<C>>),
    Effect.catchTag("StoreRefused", (error) => Effect.succeed<StoreResult<never>>({ ok: false, refusal: error.refusal })),
  );
}).pipe(Effect.withSpan("ConfigStore.call"));

/**
 * The dashboard's way into the Store: one read or write as `actor`, in the Organization named by `organizationSlug`
 * when the actor is a member of it (the same Organization gate as the Org Store's reads).
 */
export const callStoreAsMember = <C extends StoreCall>(actor: Actor, organizationSlug: string, call: C) => Effect.gen(function* () {
  const access = yield* requireAccess(actor, organizationSlug, "operate");
  const result = yield* callStore(access.organization.id, actor.userId, call);
  return access.kind === "support" ? answerForSupport(call, result, access.shows) : result;
}).pipe(Effect.withSpan("ConfigStore.callAsMember"));

/**
 * The dashboard's write: `command` as `actor`, answered with the committed `diff`, `services` and Settings views of the
 * Environment it names, so the review's rows, count, pink and values arrive with the write. A view that can't be read is left
 * out (the writer's refetch brings it); a command naming no Environment, or several (a Sync), carries none.
 */
export const writeStoreAsMember = (actor: Actor, organizationSlug: string, command: ConfigCommand) => Effect.gen(function* () {
  const access = yield* requireAccess(actor, organizationSlug, "operate");
  const { organization } = access;
  const call = { operation: "write", command } as const;
  const stored = yield* callStore(organization.id, actor.userId, call);
  const result = access.kind === "support" ? answerForSupport(call, stored, access.shows) : stored;
  const environment = commandEnvironment(command);
  if (!result.ok || environment === null) return result satisfies StoreWriteResult;
  const views = yield* Effect.all({
    diff: readStore(organization.id, { query: "diff", environment }).pipe(Effect.option),
    services: readStore(organization.id, { query: "services", environment }).pipe(Effect.option),
    environment: readStore(organization.id, { query: "environment", environment, path: null, all: true }).pipe(Effect.option),
  }, { concurrency: 3 });
  const shown = <V extends ConfigView>(view: V): V => access.kind === "support" ? viewForSupport(view, access.shows) : view;
  const committed: CommittedViews = {};
  if (Option.isSome(views.diff)) committed.diff = shown(views.diff.value);
  if (Option.isSome(views.services)) committed.services = shown(views.services.value);
  if (Option.isSome(views.environment)) committed.environment = shown(views.environment.value);
  return { ...result, views: committed } satisfies StoreWriteResult;
}).pipe(Effect.map((answer): StoreWriteResult => answer), Effect.withSpan("ConfigStore.writeAsMember"));
