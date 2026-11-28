import "@tanstack/react-start/server-only";
import type { ConfigCommand, ConfigCommitted, ConfigQuery, ConfigTrusted, ConfigWritten, DeploymentSummary, EnvironmentRef as StoreEnvironmentRef, PullRequestRef, SystemEvent } from "@ployz/sdk";
import { eq } from "drizzle-orm";
import { Effect } from "effect";
import { gatherDomainEvidence, systemDomainEvidence } from "#/modules/config-store/domain-evidence.server";
import { gatherGitEvidence } from "#/modules/config-store/git-evidence.server";
import { gatherPreviewHomes, gatherSigners, observeHomes } from "#/modules/config-store/preview-evidence.server";
import { gatherRunning } from "#/modules/config-store/running-evidence.server";
import { connectionsLoader, gatherUnanswered, gatherVolumeEvidence, systemVolumeEvidence } from "#/modules/config-store/volume-evidence.server";
import type { Actor } from "#/modules/identity/actor";
import { user } from "#/modules/identity/tables";
import { guardProtected } from "#/modules/config-store/protected.server";
import { resolvePreferredBuilders } from "#/modules/config-store/preferred-builder.server";
import { cloudStore, storeTry } from "#/modules/config-store/store-sdk.server";
import { cancelDeploymentOwedRuns } from "#/modules/config-store/store-github-builds.server";
import { githubMembers } from "#/modules/config-store/store-sources.server";
import { commandEnvironments, isResealPending, MAX_SHOWN_VIEWS } from "#/modules/config-store/store.contract";
import { StoreRefused } from "#/lib/refusal";
import { type Access, requireAccess } from "#/modules/organization/access.server";
import type { Permission } from "#/modules/organization/permissions";
import { answerForSupport } from "#/modules/support-access/support-view";
import { sendInngestEvent } from "#/modules/inngest/client";
import { variablesResealRequested } from "#/modules/sealing/sealing.events";
import { type ConfigDeploymentAdmittedEventData, type DeploymentTrigger, configDeploymentAdmitted, configSweepRequested, createConfigDeploymentStartedEvent, createConfigPrCheckRequestedEvent } from "#/modules/config-store/config-store.events";
import { Database } from "#/server/database.server";
import { countOrganizationMachines } from "#/modules/runtime/connections.server";
import { readClusterSealing } from "#/modules/sealing/cluster-sealing.server";
import { writeAdmitted } from "#/modules/volume-placement/volume-placement.server";
import type { StoreAnswer, StoreWriteResult, StoreCall, StoreRead, StoreResult, ViewAnswer } from "./store.contract";
import { environmentKey } from "./environment-ref";

/** One view Cloud reads for itself, with no evidence: its own lookups, such as a Deployment's Namespace for its logs. */
export const readStore = <Q extends ConfigQuery>(organizationId: string, query: Q) =>
  Effect.flatMap(cloudStore, (store) => storeTry(() => store.read(organizationId, query))).pipe(
    Effect.withSpan("ConfigStore.read"),
  );

/** The commands that admit Deployments: Cloud's worker runs them, and the Store refuses one nothing could run. */
const ADMITS: ReadonlySet<ConfigCommand["command"]> = new Set(["admit", "start", "remove", "approve_pull_request", "deploy_preview"]);

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
    const event = started ? createConfigDeploymentStartedEvent(data) : configDeploymentAdmitted.create(data);
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

/** Who a user's write is by, as the Store records it (a Deployment's `admitted_by`): their name, else email. */
const principalFor = Effect.fn("ConfigStore.principalFor")(function* (userId: string | null) {
  if (userId === null) return null;
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ name: user.name, email: user.email }).from(user).where(eq(user.id, userId)).limit(1);
  return row === undefined ? null : row.name || row.email;
});

/**
 * Which events can admit a Deployment, and so need to know which Servers hold the data a Save accepted losing, and
 * who the members are: a trusted pull request's head deploys only while its author is one, unless a bot opened it from
 * the repository itself.
 */
const deploys = {
  branch_head: true, check_suite: true, pull_request: true, sweep: false, cluster_forgotten: false, volume_resized: false,
} satisfies Record<SystemEvent["event"], boolean>;

/** Which events can open or close a preview: a pull request's, and the sweep that moves one off a Server that left. */
const places = {
  branch_head: false, check_suite: false, pull_request: true, sweep: true, cluster_forgotten: false, volume_resized: false,
} satisfies Record<SystemEvent["event"], boolean>;

/**
 * Something Cloud observed, told to the Organization's Store with how many Servers could run what it admits, the
 * hostnames other Namespaces publish, and, for an event that can deploy, which Servers hold the data a Save accepted
 * losing and who the members are.
 */
export const storeSystem = Effect.fn("ConfigStore.system")(function* (organizationId: string, event: SystemEvent) {
  const store = yield* cloudStore;
  const servers = yield* countOrganizationMachines(organizationId);
  const domains = yield* systemDomainEvidence(organizationId);
  const volumes = deploys[event.event]
    ? yield* systemVolumeEvidence(yield* connectionsLoader(organizationId), (query) => store.read(organizationId, query))
    : undefined;
  const trusted: Pick<ConfigTrusted, "servers" | "domains" | "volumes" | "members" | "preview_homes"> = { servers, domains };
  if (volumes !== undefined) trusted.volumes = volumes;
  if (deploys[event.event]) trusted.members = yield* githubMembers(organizationId);
  // A preview's homes come with the members, since moving one opens it again for its pull request.
  const homes = places[event.event] ? yield* gatherPreviewHomes(organizationId, (query) => store.read(organizationId, query)) : undefined;
  if (homes !== undefined) Object.assign(trusted, homes);
  return yield* storeTry(() => store.system(organizationId, event, trusted));
});

/**
 * The trusted evidence `call` needs, as Cloud observes it itself: GitHub's for repository Services, what Cloud observes
 * of domains, for a Save or Deploy that removes deployed Volumes what the Servers hold of them, for a full Deploy that prunes
 * which Services they run, for a removal which Servers didn't answer, for an admission or start how many Servers could run it, and for a write the Cluster's Sealing key a
 * generated secret is sealed to. Nothing here comes from the caller. Refused `unavailable` when GitHub or the Cluster Domain
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
      code: "unavailable", message: "Cloud couldn't reserve the Cluster Domain; try again.", details: null,
    }))),
  );
  const connections = yield* connectionsLoader(organizationId);
  const volumes = yield* gatherVolumeEvidence(connections, call, read);
  const unanswered = yield* gatherUnanswered(connections, call, volumes);
  const running = yield* gatherRunning(connections, call);
  // An admission or start carries how many Servers the Organization has enrolled, so the Store refuses one nothing could run.
  const admits = call.operation === "write" && ADMITS.has(call.command.command);
  const servers = admits ? yield* countOrganizationMachines(organizationId) : undefined;
  // An approval leaves a pull request trusted only while its author is a member or a bot opened it from the repository.
  const approves = call.operation === "write" && call.command.command === "approve_pull_request";
  // A preview deploys only a member's pull request, on a Server that can hold one. The previews view names the
  // members too, but asks no Server: it refreshes as the Store changes. A preview rule's draft lists every Server that
  // can hold one, even before the Organization uses previews.
  const deploysPreview = call.operation === "write" && call.command.command === "deploy_preview";
  const listsPreviews = call.operation === "read" && call.query.query === "previews";
  const members = approves || deploysPreview || listsPreviews ? yield* githubMembers(organizationId) : undefined;
  const drafts = call.operation === "read" && call.query.query === "preview_rule";
  const homes = deploysPreview ? yield* gatherPreviewHomes(organizationId, read) : drafts ? yield* observeHomes(organizationId) : undefined;
  // A preview rule is kept, and a commit approval deploys a preview, only once a key the Servers trust signed it.
  const signs = call.operation === "write" && (
    (call.command.command === "set_pr_plan" && call.command.preview_rule != null)
    || (call.command.command === "deploy_preview" && call.command.approval != null));
  const signers = signs ? yield* gatherSigners(organizationId) : undefined;
  const sealing = call.operation === "write" ? yield* readClusterSealing(organizationId) : undefined;
  const trusted: ConfigTrusted = { ...git, domains };
  // Values the Store seals itself go where the browser's do: to the recipient key once the Cluster activated.
  if (sealing?.state === "ready") trusted.sealing = { publicKey: sealing.publicKey };
  if (volumes !== undefined) trusted.volumes = volumes;
  if (unanswered !== undefined) trusted.unanswered = unanswered;
  if (running !== undefined) trusted.running = running;
  if (servers !== undefined) trusted.servers = servers;
  if (members !== undefined) trusted.members = members;
  if (homes !== undefined) trusted.preview_homes = homes.preview_homes;
  if (signers !== undefined) trusted.signers = signers;
  return trusted;
});

/** What a committed write leaves Cloud to do: cancel the GitHub runs a cancelled Deployment owes, run an admitted one, recheck PRs. */
const afterWrite = Effect.fn("ConfigStore.afterWrite")(function* (
  organizationId: string, command: ConfigCommand, written: ConfigCommitted,
) {
  // The cancel ended its GitHub builds in its own write, owing each run Cloud dispatched a cancel.
  if (command.command === "cancel") {
    yield* cancelDeploymentOwedRuns(command.deployment).pipe(
      Effect.catch((error) => Effect.logWarning("Could not stop a cancelled Deployment's GitHub builds.", error)),
    );
  }
  // An admitted (or retried) or started Deployment goes to Cloud's worker, whoever asked, as do a removal's first ones
  // and an approved pull request's.
  if (ADMITS.has(command.command)) {
    yield* dispatchAdmitted(organizationId, written, command.command === "start");
  }
  // A removal that applied at admission (nothing on a Server) is deleted by the next sweep, and admits what is due
  // after it, as is the removal a Sync's close marked: no worker will ask for that sweep, so ask now.
  const removed = written.written === "deployment" ? written.remove && written.status === "applied"
    : written.written === "removal" ? written.removal === "removing" && written.left.some((left) => left.deployment?.status === "applied")
    : written.written === "automated" ? written.closing.length > 0
    : written.written === "synced" && written.closing;
  if (removed) yield* sendInngestEvent(configSweepRequested.create({ organizationId }));
  yield* requestChecks(organizationId, written.checks);
});

/**
 * One Store read or write by user `userId` (null: Cloud itself) as `organizationId`: the answer, or the Store's refusal verbatim. It first
 * gathers the trusted evidence the call needs (`gatherTrusted`), and a Deploy meets the Volume copies it takes
 * (`writeAdmitted`) before the Store sees it; `gate` is what a human approved and which Deploys ask, which only a CLI
 * or agent write in an Organization that asks before destructive actions carries. Anything else (the Store failing to open, a broken
 * binding) is a defect.
 */
export const callStore = <C extends StoreCall>(
  organizationId: string,
  userId: string | null,
  asked: C,
  gate: Pick<ConfigTrusted, "approval" | "asks" | "permissions" | "protector" | "environments" | "from_cli"> = { approval: "not_required" },
) => Effect.gen(function* () {
  const store = yield* cloudStore;
  const read: StoreRead = (query) => store.read(organizationId, query);
  return yield* Effect.gen(function* () {
    // Every surface's edit passes here, so a Server named as a Preferred Builder reaches the Store as its Machine ID.
    const call: StoreCall = asked.operation === "write"
      ? { operation: "write", command: yield* resolvePreferredBuilders(organizationId, asked.command) }
      : asked;
    if (call.operation === "write") yield* guardProtected(gate.permissions, call.command, read);
    const trusted: ConfigTrusted = { ...yield* gatherTrusted(organizationId, call, read), ...gate };
    // Who acts by their immutable id: a Deployment is their own by it, never by name, which may repeat.
    if (userId !== null) trusted.user = userId;
    if (call.operation === "read") return { ok: true, value: yield* storeTry(() => store.read(organizationId, call.query, trusted)) };
    // Cloud sends every user's set, so a write by a user with none named is a defect, never a grant.
    if (userId !== null && gate.permissions === undefined) return yield* Effect.die("A user's Store write names no permissions");
    const principal = yield* principalFor(userId).pipe(Effect.orDie);
    const written = yield* writeAdmitted(organizationId, call.command, (command) =>
      storeTry(() => store.write(organizationId, command, trusted, principal)));
    // The write committed: nothing after it may answer as its refusal. What failed is logged; sweeps redo it.
    yield* afterWrite(organizationId, call.command, written).pipe(
      Effect.catchCause((cause) => Effect.logWarning("Cloud's follow-up to a committed Store write failed.", cause)),
    );
    return { ok: true, value: written };
  }).pipe(
    // SAFETY: a read answers its query's view, a write what it wrote.
    Effect.map((result) => result as StoreResult<StoreAnswer<C>>),
    Effect.catchTag("StoreRefused", (error) => Effect.gen(function* () {
      // A Deploy waits for the reseal of a Legacy value: ask for it now rather than at the next worker start.
      if (isResealPending(error.refusal)) {
        yield* sendInngestEvent(variablesResealRequested.create({ organizationId })).pipe(
          Effect.catchCause((cause) => Effect.logWarning("A reseal request for a refused Deploy failed.", cause)),
        );
      }
      return { ok: false, refusal: error.refusal } satisfies StoreResult<never>;
    })),
  );
}).pipe(Effect.withSpan("ConfigStore.call"));

/**
 * The dashboard's way into the Store: one read or write as `actor`, in the Organization named by `organizationSlug`
 * when the actor is a member of it (the same Organization gate as the Org Store's reads).
 */
export const callStoreAsMember = <C extends StoreCall>(actor: Actor, organizationSlug: string, call: C) => Effect.gen(function* () {
  const access = yield* requireAccess(actor, organizationSlug, "project:read");
  return yield* callStoreWith(access, actor, call);
}).pipe(Effect.withSpan("ConfigStore.callAsMember"));

const callStoreWith = <C extends StoreCall>(access: Access, actor: Actor, call: C) =>
  Effect.map(callStore(access.organization.id, actor.userId, call, { approval: "not_required", permissions: access.permissions, environments: access.environments }), (result) =>
    access.kind === "support" ? answerForSupport(call, result, access.shows) : result);

/**
 * The dashboard's write: `command` as `actor`, answered with the views the tab shows of the Environments it reads or
 * writes (`shows`), each read after the commit exactly as the tab's own read of it, so every view it moved arrives with
 * the write. A query of another Environment, past `MAX_SHOWN_VIEWS`, or whose reread fails, is dropped; the tab refetches those.
 */
export const writeStoreAsMember = (actor: Actor, organizationSlug: string, command: ConfigCommand, shows: readonly ConfigQuery[] = []) => Effect.gen(function* () {
  const access = yield* requireAccess(actor, organizationSlug, writePermission(command), "store");
  const result = yield* callStoreWith(access, actor, { operation: "write", command });
  if (!result.ok) return result satisfies StoreWriteResult;
  const keys = new Set(commandEnvironments(command).map(environmentKey));
  const asked = shows.filter((query) => queryEnvironments(query).some((ref) => keys.has(environmentKey(ref)))).slice(0, MAX_SHOWN_VIEWS);
  // The write committed, so a reread that fails (a database read its evidence needs) only drops that view's answer.
  const answers = yield* Effect.forEach(asked, (query) =>
    callStoreWith(access, actor, { operation: "read", query }).pipe(
      Effect.map((answer): ViewAnswer[] => [{ query, result: answer }]),
      Effect.catchCause((cause) => Effect.logWarning("A view reread after a committed write failed; the tab refetches it.", cause).pipe(
        Effect.as<ViewAnswer[]>([]),
      )),
    ),
  { concurrency: SHOWN_VIEW_READS });
  return { ...result, answers: answers.flat() } satisfies StoreWriteResult;
}).pipe(Effect.map((answer): StoreWriteResult => answer), Effect.withSpan("ConfigStore.writeAsMember"));

/**
 * Deleting an Environment or a Project takes its delete permission, protecting one its own, the Build Order the
 * Organization's settings, and naming a region operating Servers, as the Store asks; every other write only staging.
 */
export const writePermission = (command: ConfigCommand): Permission => {
  if (command.command === "set_environment_protected") return "environments:protect";
  if (command.command === "set_build_order") return "org:settings";
  if (command.command === "name_region") return "servers:operate";
  if (command.command === "deploy_preview" || command.command === "close_preview") return "changes:deploy";
  if (command.command !== "remove") return "changes:stage";
  return command.environment == null ? "project:delete" : "environment:delete";
};

const SHOWN_VIEW_READS = 4;

function queryEnvironments(query: ConfigQuery): StoreEnvironmentRef[] {
  if (query.query === "sync") return query.into ? [query.from, query.into] : [query.from];
  return "environment" in query && query.environment ? [query.environment] : [];
}
