import { it } from "@effect/vitest";
import type { RuntimeWatchView } from "@ployz/sdk";
import { eq, sql } from "drizzle-orm";
import { Effect, Option, Schema } from "effect";
import { expect, vi } from "vitest";
import type { JsonObject } from "#/db/tables";
import { asTestDouble } from "#/lib/test-double";
import type { AgentCaller } from "#/modules/identity/actor";
import { runAsAgent } from "#/test/agent-call";
import type { ToolOutcome } from "#/modules/agent/agent";
import { findService } from "#/modules/config-store/store-lookup.server";
import type { Caller } from "#/modules/identity/actor";
import { session, user } from "#/modules/identity/tables";
import { member } from "#/modules/organization/tables";
import {
  advanceProposal,
  fillDeploy,
  createProposal,
  asSeenBy,
  decideProposal,
  pendingProposals,
  presentProposal,
  proposalStatus,
  stageStep,
  sweepProposals,
} from "#/modules/proposals/proposals.server";
import { DEPLOY_MOVED } from "#/modules/proposals/proposal";
import type { CliCall } from "#/modules/commands/cli-call.server";
import { proposalsCliRoutes } from "#/modules/proposals/proposals.cli.server";
import { operationProposals } from "#/modules/proposals/tables";
import type { Change, ConfigCommand, ConfigQuery, ConfigStore, ConfigTrusted, DiffView, MachineId, SavedRule } from "@ployz/sdk";
import { RpcError } from "#/modules/runtime/ployz.server";
import { admitDeploy } from "#/modules/config-store/admit-deploy";
import { readStore } from "#/modules/config-store/config-store.server";
import { RunningObserver } from "#/modules/config-store/running-evidence.server";
import { envPath } from "#/modules/config-store/setting-paths";
import { type DeployLink, linkDeploy } from "#/modules/deploy-approval/deploy-link.server";
import { prepareDeployTap } from "#/modules/deploy-approval/deploy-tap.server";
import { createOrganizationToken, revokeCredential } from "#/modules/identity/organization-token.server";
import { FULL_GRANT } from "#/modules/identity/token-grant";
import { OrganizationRuntime, type OrganizationRuntimeService } from "#/modules/runtime/organization-runtime.server";
import type { PloyzSession } from "#/modules/runtime/ployz.server";
import { serviceAct } from "#/modules/service-act/tables";
import { AuthLive } from "#/modules/identity/auth.server";
import { Database } from "#/server/database.server";
import { acmeWeb, enrollStoreServer, seedStoreGitService, seedStoreOrganization, storeTestCloudWith } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000b301";
const PROJECT = "00000000-0000-4000-8000-00000000b303";
const ENVIRONMENT = "00000000-0000-4000-8000-00000000b304";
const SERVICE = "00000000-0000-4000-8000-00000000b305";
const BROWSER = "00000000-0000-4000-8000-00000000b3a0";
const ADA_BROWSER = "00000000-0000-4000-8000-00000000b3a1";
const ASKED = "00000000-0000-4000-8000-00000000b3b0";
const ASKED_AGAIN = "00000000-0000-4000-8000-00000000b3b1";
const DEPLOYMENT = "00000000-0000-4000-8000-00000000b306";
const CONTAINER = "c".repeat(64);
const MACHINE = "b".repeat(32);
const production = { project: "shop", environment: "production" };
const page = { at: "here", ...production } as const;

const Created = Schema.Struct({ ok: Schema.Literal(true), value: Schema.Struct({ proposal_id: Schema.String }) });
const idOf = (result: ToolOutcome) =>
  Schema.decodeUnknownOption(Created)(result).pipe(Option.map((created) => created.value.proposal_id), Option.getOrElse(() => expect.fail("no proposal")));

/** Shop with `web`, an agent acting for its owner, and a member who can approve from the dashboard. */
const shop = Effect.fn(function* () {
  const services = yield* storeTestCloudWith(AuthLive);
  const provided = <A, E, R>(effect: Effect.Effect<A, E, R>) => effect.pipe(Effect.provide(services));
  const userId = yield* provided(seedStoreOrganization(ORGANIZATION));
  const store = yield* provided(seedStoreGitService(ORGANIZATION, { project: PROJECT, environment: ENVIRONMENT, service: SERVICE }));
  // `web` deployed, its Applied State written as a finished Deployment would, so the Servers know it by name.
  yield* Effect.promise(() => store.write(ORGANIZATION, {
    command: "admit", admit: "deploy", id: DEPLOYMENT, environment: { project: null, environment: null }, services: [], version: null, accept_volume_loss: [],
  }, acmeWeb()));
  yield* provided(Effect.flatMap(Database, ({ drizzle }) => Effect.gen(function* () {
    yield* drizzle.execute(sql`update config_deployment set status = 'applied' where id = ${DEPLOYMENT}`);
    yield* drizzle.execute(sql`insert into config_applied (environment_id, node_id, organization_id, deployment_id, node_type, node)
      select environment_id, service->>'id', organization_id, ${DEPLOYMENT}, 'service', service::text
      from config_saved, lateral jsonb_array_elements(intent::jsonb->'services') service where environment_id = ${ENVIRONMENT}`);
  })));
  const found = (yield* provided(findService(ORGANIZATION, ENVIRONMENT, "web"))) ?? expect.fail("no web");
  const protect = Effect.promise(() => store.write(ORGANIZATION, { command: "set_environment_protected", environment: { project: null, environment: null }, protected: true }));
  const agent: AgentCaller = { userId, organization: { id: ORGANIZATION, slug: "shop" }, credential: { kind: "session", id: BROWSER }, role: "owner", support: null, via: "agent" };
  const owner: Caller = { ...agent, via: "dashboard" };
  const capabilities: string[] = [];
  let readable = true;
  let enforcing = true;
  let running: string[] = [];
  const frame = () => asTestDouble<RuntimeWatchView>()({
    services: [{ identity: `${found.namespace}/${found.runtime}`, containers: [{ container_id: CONTAINER, machine_id: MACHINE }], hook_containers: [] }],
    machines: [{ machine: { id: MACHINE, name: "machine-2", runtime: { device_enforcement: enforcing, capabilities } } }],
    containers: [],
    incomplete_ids: { machines: [] },
  });
  const cluster = <A, E, R>(effect: Effect.Effect<A, E, R>) => provided(effect.pipe(Effect.provideService(
    OrganizationRuntime,
    asTestDouble<OrganizationRuntimeService>()({
      open: () => readable
        ? Effect.succeed({ status: "connected" as const, connected: asTestDouble<PloyzSession>()({ watchFirstFrame: () => Effect.succeed(frame()) }) })
        : Effect.succeed({ status: "unreachable" as const }),
    }),
  ), Effect.provideService(RunningObserver, { observe: async () => ({ services: running, unanswered: [] }) })));
  const acts = provided(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    return yield* drizzle.select({ id: serviceAct.id, verb: serviceAct.verb, step: serviceAct.proposalStep, proposal: serviceAct.proposalId, key: serviceAct.state })
      .from(serviceAct).orderBy(serviceAct.requestedAt);
  }));
  const ended = (actId: string, state: "done" | "failed") => provided(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* drizzle.update(serviceAct).set({ state, endedAt: new Date() }).where(eq(serviceAct.id, actId));
  }));
  const call = (tool: string, input: JsonObject, key = `call-${Math.random()}`) =>
    cluster(runAsAgent(agent, { tool, input, key, page })).pipe(Effect.map((result) => result.outcome));
  const staged = Effect.gen(function* () {
    const id = idOf(yield* call("proposal_create", {}));
    expect(yield* call("service_stop", { service: "web", proposal: id })).toMatchObject({ ok: true, value: { step: 1 } });
    expect(yield* call("service_start", { service: "web", proposal: id })).toMatchObject({ ok: true, value: { step: 2 } });
    return id;
  });
  const approve = { approve: {} };
  /** Servers that don't enforce device approval: what a Deploy needs is only the agent gate's person. */
  const enforce = (on: boolean) => { enforcing = on; };
  /** The Services the Servers run in shop/production besides what it holds, as a full Deploy's prune sees them. */
  const runs = (names: readonly string[]) => { running = names.map((name) => `${found.namespace}/${name}`); };
  /** How many Deployments the Store holds with the id the proposal's Deploy admits. */
  const admitted = (proposal: string) => provided(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const [row] = yield* drizzle.select({ steps: operationProposals.steps }).from(operationProposals).where(eq(operationProposals.id, proposal));
    const [step] = row?.steps ?? [];
    const id = step !== undefined && "deploy" in step ? step.deploy.command.id : expect.fail("not a deploy");
    return (yield* drizzle.execute(sql`select id from config_deployment where id = ${id}`, "objects")).length;
  }));
  const write = (command: ConfigCommand) => Effect.promise(() => store.write(ORGANIZATION, command, acmeWeb()));
  return { provided, cluster, agent, owner, found, acts, ended, call, staged, approve, protect, enforce, runs, write, admitted, store };
});

it.live("an agent stages steps without running any, presents them, and the person sees them in order", () =>
  Effect.gen(function* () {
    const { call, staged, acts, provided, agent } = yield* shop();
    const id = yield* staged;
    expect(yield* acts).toEqual([]);
    // Only the three Service tools stage, and a staged step takes no approval.
    expect(yield* call("deploy", { proposal: id })).toMatchObject({ ok: false, refusal: { code: "invalid_argument", message: "Only service_restart, service_stop and service_start can be staged into a proposal. Call deploy without proposal." } });
    expect(yield* provided(stageStep(agent, id, { environment: "00000000-0000-4000-8000-00000000b3ee", service: "web", verb: "stop", only: null })))
      .toMatchObject({ ok: false, refusal: { code: "invalid_argument", message: "This proposal runs in shop/production. Stage steps for that environment, or create a proposal for this one." } });
    expect(yield* call("service_stop", { service: "api", proposal: id })).toMatchObject({ ok: false, refusal: { code: "not_found" } });
    expect(yield* call("service_stop", { service: "web", proposal: "00000000-0000-4000-8000-00000000b3ff" })).toMatchObject({ ok: false, refusal: { code: "not_found" } });
    expect(yield* provided(pendingProposals(ORGANIZATION))).toEqual([]);
    expect(yield* call("proposal_status", { proposal: id })).toMatchObject({ ok: true, value: { status: "draft", steps: [{ index: 1, verb: "stop", state: "not_run" }, { index: 2, verb: "start", state: "not_run" }] } });
    expect(yield* call("proposal_present", { proposal: id })).toMatchObject({ ok: true, value: { steps: 2 } });
    expect(yield* call("service_restart", { service: "web", proposal: id })).toMatchObject({ ok: false, refusal: { code: "conflict" } });
    expect(yield* call("proposal_present", { proposal: id })).toMatchObject({ ok: false, refusal: { code: "conflict" } });
    const [pending] = yield* provided(pendingProposals(ORGANIZATION));
    expect(pending).toMatchObject({ id, status: "presented", project: "shop", environment: "production", requested_by: agent.userId, steps: [
      { verb: "stop", service: "web", only: null, namespace: expect.any(String), runtime: "web" },
      { verb: "start", service: "web", only: null },
    ] });
    // Someone else's draft can't be added to.
    const other = yield* provided(createProposal({ ...agent, credential: { kind: "oauth", id: "cli-oauth", client: null } }, production));
    const otherId = other.ok && "value" in other ? other.value.proposal_id : expect.fail("no proposal");
    expect(yield* provided(stageStep(agent, otherId, { environment: ENVIRONMENT, service: "web", verb: "stop", only: null }))).toMatchObject({ ok: false, refusal: { code: "forbidden" } });
    expect(yield* provided(presentProposal(agent, otherId))).toMatchObject({ ok: false, refusal: { code: "forbidden" } });
  }));

it.live("an approval runs the first step, each done step starts the next, and a failed one stops the rest", () =>
  Effect.gen(function* () {
    const { call, staged, acts, cluster, provided, owner, ended, approve } = yield* shop();
    const id = yield* staged;
    yield* call("proposal_present", { proposal: id });
    expect(yield* cluster(decideProposal(owner, id, approve))).toMatchObject({ ok: true, proposal: { status: "approved" } });
    let rows = yield* acts;
    expect(rows).toMatchObject([{ verb: "stop", step: 0, proposal: id }]);
    expect(yield* provided(proposalStatus(owner, id))).toMatchObject({ ok: true, value: { status: "approved", steps: [{ state: "accepted" }, { state: "waiting" }] } });
    // Approving again, or denying, answers that it was decided and runs nothing more.
    expect(yield* cluster(decideProposal(owner, id, approve))).toMatchObject({ ok: false, message: "This proposal was already decided elsewhere (approved)." });
    expect(yield* cluster(decideProposal(owner, id, { reject: {} }))).toMatchObject({ ok: false, message: expect.stringContaining("already decided") });
    expect(yield* acts).toHaveLength(1);
    // The run's end is what starts the next step; a replay of that end admits the same step once.
    const first = rows[0]?.id ?? expect.fail("no act");
    yield* ended(first, "done");
    yield* cluster(advanceProposal({ actId: first, organizationId: ORGANIZATION }));
    yield* cluster(advanceProposal({ actId: first, organizationId: ORGANIZATION }));
    rows = yield* acts;
    expect(rows).toMatchObject([{ verb: "stop", step: 0 }, { verb: "start", step: 1, proposal: id }]);
    const second = rows[1]?.id ?? expect.fail("no second act");
    yield* ended(second, "done");
    yield* cluster(advanceProposal({ actId: second, organizationId: ORGANIZATION }));
    expect(yield* acts).toHaveLength(2);
    expect(yield* provided(proposalStatus(owner, id))).toMatchObject({ ok: true, value: { steps: [{ state: "done" }, { state: "done" }] } });
  }));

it.live("a step with more than one run reads as its newest", () =>
  Effect.gen(function* () {
    const { call, staged, acts, cluster, provided, owner, ended, approve } = yield* shop();
    const id = yield* staged;
    yield* call("proposal_present", { proposal: id });
    expect(yield* cluster(decideProposal(owner, id, approve))).toMatchObject({ ok: true });
    const [first] = yield* acts;
    yield* ended(first?.id ?? expect.fail("no act"), "failed");
    const again = (id: string, state: "done" | "failed", later: number) => provided(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      const [run] = yield* drizzle.select().from(serviceAct).where(eq(serviceAct.id, first?.id ?? ""));
      if (run === undefined) return expect.fail("no run");
      yield* drizzle.insert(serviceAct).values({ ...run, id, state, reason: null, inngestRunId: null, endedAt: new Date(), requestedAt: new Date(run.requestedAt.getTime() + later) });
    }));
    yield* again("00000000-0000-4000-8000-00000000b3a1", "done", 1_000);
    expect(yield* provided(proposalStatus(owner, id))).toMatchObject({ ok: true, value: { steps: [{ state: "done" }, { state: "waiting" }] } });
    yield* again("00000000-0000-4000-8000-00000000b3a2", "failed", 2_000);
    expect(yield* provided(proposalStatus(owner, id))).toMatchObject({ ok: true, value: { steps: [{ state: "failed" }, { state: "not_run" }] } });
  }));

// Catches a member's approval, given while production was unprotected, still restarting web after an admin protected it.
it.live("a step whose Environment was protected after a member approved halts for an owner or admin", () =>
  Effect.gen(function* () {
    const { call, acts, cluster, provided, owner, ended, approve, protect, agent, write } = yield* shop();
    yield* write({ command: "set_environment_protected", environment: { project: null, environment: null }, protected: false });
    const id = idOf(yield* call("proposal_create", {}));
    yield* call("service_stop", { service: "web", proposal: id });
    yield* call("service_restart", { service: "web", proposal: id });
    yield* call("proposal_present", { proposal: id });
    const bo = yield* provided(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      const [person] = yield* drizzle.insert(user).values({ email: "bo.member@example.test", name: "Bo" }).returning();
      yield* drizzle.insert(member).values({ userId: person?.id ?? "", organizationId: ORGANIZATION, role: "member" });
      return { ...owner, userId: person?.id ?? expect.fail("no person"), role: "member" } satisfies Caller;
    }));
    expect(yield* cluster(decideProposal(bo, id, approve))).toMatchObject({ ok: true, proposal: { status: "approved" } });
    const [first] = yield* acts;

    yield* protect;
    yield* ended(first?.id ?? expect.fail("no act"), "done");
    yield* cluster(advanceProposal({ actId: first?.id ?? "", organizationId: ORGANIZATION }));
    expect(yield* acts).toHaveLength(1);
    expect(yield* provided(proposalStatus(agent, id))).toMatchObject({ ok: true, value: {
      steps: [{ state: "done" }, { state: "failed", reason: "production is Protected now. An owner or admin must approve it." }],
    } });
  }));

it.live("a step that fails leaves the ones after it unrun", () =>
  Effect.gen(function* () {
    const { call, staged, acts, cluster, provided, owner, ended, approve } = yield* shop();
    const id = yield* staged;
    yield* call("proposal_present", { proposal: id });
    expect(yield* cluster(decideProposal(owner, id, approve))).toMatchObject({ ok: true });
    const [first] = yield* acts;
    yield* ended(first?.id ?? expect.fail("no act"), "failed");
    yield* cluster(advanceProposal({ actId: first?.id ?? "", organizationId: ORGANIZATION }));
    expect(yield* acts).toHaveLength(1);
    expect(yield* provided(proposalStatus(owner, id))).toMatchObject({ ok: true, value: { steps: [{ state: "failed" }, { state: "not_run" }] } });
  }));

it.live("a first step that can't start hands the proposal back to waiting", () =>
  Effect.gen(function* () {
    const { call, staged, acts, cluster, provided, owner, approve, agent } = yield* shop();
    const id = yield* staged;
    yield* call("proposal_present", { proposal: id });
    // The asker leaves; another owner approves.
    const bob = yield* provided(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      const [person] = yield* drizzle.insert(user).values({ email: "bob@example.test", name: "Bob" }).returning();
      yield* drizzle.insert(member).values({ userId: person?.id ?? "", organizationId: ORGANIZATION, role: "owner" });
      yield* drizzle.delete(member).where(eq(member.userId, agent.userId));
      return person?.id ?? expect.fail("no person");
    }));
    expect(yield* cluster(decideProposal({ ...owner, userId: bob }, id, approve))).toMatchObject({ ok: false, message: expect.stringMatching(/^It didn't run: .* It's still waiting for approval\.$/) });
    expect(yield* acts).toEqual([]);
    expect(yield* provided(pendingProposals(ORGANIZATION))).toMatchObject([{ id, status: "presented" }]);
    const [row] = yield* provided(Effect.flatMap(Database, ({ drizzle }) => drizzle.select({ halt: operationProposals.halt }).from(operationProposals).where(eq(operationProposals.id, id))));
    expect(row).toEqual({ halt: null });
  }));

// Catches a member who can't protect production approving their own stop there, or their ask refused outright.
it.live("a member's stop in a Protected Environment is presented, and only someone who could protect it approves it", () =>
  Effect.gen(function* () {
    const { provided, cluster, agent, owner, approve, protect } = yield* shop();
    const [person] = yield* provided(Effect.flatMap(Database, ({ drizzle }) => drizzle.insert(user).values({ email: "ada.member@example.test", name: "Ada Member" }).returning()));
    yield* provided(Effect.flatMap(Database, ({ drizzle }) => drizzle.insert(member).values({ userId: person?.id ?? "", organizationId: ORGANIZATION, role: "member" })));
    const ada: AgentCaller = { ...agent, userId: person?.id ?? "", credential: { kind: "session", id: "ada-session" }, role: "member" };
    const adas = yield* provided(createProposal(ada, production));
    const adaId = adas.ok && "value" in adas ? adas.value.proposal_id : expect.fail("no proposal");
    expect(yield* provided(stageStep(ada, adaId, { environment: ENVIRONMENT, service: "web", verb: "stop", only: null }))).toMatchObject({ ok: true });
    yield* protect;
    expect(yield* provided(presentProposal(ada, adaId))).toMatchObject({ ok: true });
    expect(yield* cluster(decideProposal(ada, adaId, approve))).toEqual({ ok: false, message: "An owner or admin does this." });
    expect(yield* provided(pendingProposals(ORGANIZATION))).toMatchObject([{ id: adaId, status: "presented" }]);
    expect(yield* cluster(decideProposal(owner, adaId, approve))).toMatchObject({ ok: true, proposal: { status: "approved" } });
  }));

// Catches another member ending a protected member's ask for good, before an owner or admin could decide it.
it.live("only someone who could protect a Protected Environment rejects a member's stop there", () =>
  Effect.gen(function* () {
    const { provided, cluster, agent, owner, protect } = yield* shop();
    const memberOf = (email: string) => provided(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      const [person] = yield* drizzle.insert(user).values({ email, name: email }).returning();
      yield* drizzle.insert(member).values({ userId: person?.id ?? "", organizationId: ORGANIZATION, role: "member" });
      return { ...agent, userId: person?.id ?? "", credential: { kind: "session", id: `${email}-session` }, role: "member" } satisfies AgentCaller;
    }));
    const ada = yield* memberOf("ada.member@example.test");
    const bo = yield* memberOf("bo.member@example.test");
    yield* protect;
    const created = yield* provided(createProposal(ada, production));
    const id = created.ok && "value" in created ? created.value.proposal_id : expect.fail("no proposal");
    expect(yield* provided(stageStep(ada, id, { environment: ENVIRONMENT, service: "web", verb: "stop", only: null }))).toMatchObject({ ok: true });
    expect(yield* provided(presentProposal(ada, id))).toMatchObject({ ok: true });
    const reject = { reject: { reason: "not now" } };

    expect(yield* cluster(decideProposal(bo, id, reject))).toEqual({ ok: false, message: "An owner or admin does this." });
    expect(yield* provided(pendingProposals(ORGANIZATION))).toMatchObject([{ id, status: "presented" }]);
    expect(yield* cluster(decideProposal(owner, id, reject))).toMatchObject({ ok: true, proposal: { status: "denied", reason: "not now" } });
  }));

// Catches a member who asked for a production stop by mistake being unable to take it back, so it waits for a protector's yes.
it.live("the member who asked withdraws their own stop in a Protected Environment, and it runs nothing", () =>
  Effect.gen(function* () {
    const { provided, cluster, agent, owner, approve, protect } = yield* shop();
    const [person] = yield* provided(Effect.flatMap(Database, ({ drizzle }) => drizzle.insert(user).values({ email: "ada.member@example.test", name: "Ada Member" }).returning()));
    yield* provided(Effect.flatMap(Database, ({ drizzle }) => drizzle.insert(member).values({ userId: person?.id ?? "", organizationId: ORGANIZATION, role: "member" })));
    const ada: AgentCaller = { ...agent, userId: person?.id ?? "", credential: { kind: "session", id: "ada-session" }, role: "member" };
    yield* protect;
    const created = yield* provided(createProposal(ada, production));
    const id = created.ok && "value" in created ? created.value.proposal_id : expect.fail("no proposal");
    expect(yield* provided(stageStep(ada, id, { environment: ENVIRONMENT, service: "web", verb: "stop", only: null }))).toMatchObject({ ok: true });
    expect(yield* provided(presentProposal(ada, id))).toMatchObject({ ok: true });
    const seen = yield* provided(Effect.flatMap(pendingProposals(ORGANIZATION), (views) => asSeenBy(ORGANIZATION, ada.userId, views)));
    expect(seen).toMatchObject([{ id, mine: true, decides: false }]);

    expect(yield* cluster(decideProposal(ada, id, { reject: {} }))).toMatchObject({ ok: true, proposal: { status: "denied", withdrawn: true } });
    expect(yield* provided(pendingProposals(ORGANIZATION))).toEqual([]);
    expect(yield* cluster(decideProposal(owner, id, approve))).toMatchObject({ ok: false, message: expect.stringContaining("already decided") });
  }));

it.live("steps staged at once all land, each at its own index, and the plan holds ten at most", () =>
  Effect.gen(function* () {
    const { provided, call, agent } = yield* shop();
    const id = idOf(yield* call("proposal_create", {}));
    const stage = (verb: "stop" | "start" | "restart") => provided(stageStep(agent, id, { environment: ENVIRONMENT, service: "web", verb, only: null }));
    const landed = yield* Effect.all([stage("stop"), stage("start"), stage("restart")], { concurrency: "unbounded" });
    expect(landed.map((result) => result.ok && "value" in result ? result.value.step : expect.fail("not staged")).sort((left, right) => left - right)).toEqual([1, 2, 3]);
    expect(yield* call("proposal_status", { proposal: id })).toMatchObject({ ok: true, value: { steps: [{ index: 1 }, { index: 2 }, { index: 3 }] } });
    yield* Effect.all(Array.from({ length: 7 }, () => stage("restart")), { concurrency: "unbounded" });
    expect(yield* stage("restart")).toMatchObject({ ok: false, refusal: { code: "invalid_argument", message: "A proposal holds at most 10 steps." } });
    expect(yield* call("proposal_status", { proposal: id })).toMatchObject({ ok: true, value: { steps: expect.arrayContaining([{ index: 10, verb: "restart", service: "web", state: "not_run", reason: null }]) } });
  }));

it.live("a token limited to other Environments can't open or add to a proposal here, and a step rechecks that before it runs", () =>
  Effect.gen(function* () {
    const { provided, cluster, call, agent, owner, acts, approve, staged } = yield* shop();
    const elsewhere: AgentCaller = { ...agent, credential: { kind: "token", id: "token-staging", grant: { permissions: null, environments: ["staging"] } } };
    expect(yield* provided(createProposal(elsewhere, production))).toMatchObject({ ok: false, refusal: { code: "forbidden", details: { environments: ["staging"] } } });
    const here: AgentCaller = { ...agent, credential: { kind: "token", id: "token-production", grant: { permissions: null, environments: ["production"] } } };
    const opened = yield* provided(createProposal(here, production));
    const hereId = opened.ok && "value" in opened ? opened.value.proposal_id : expect.fail("no proposal");
    expect(yield* provided(stageStep(here, hereId, { environment: ENVIRONMENT, service: "web", verb: "stop", only: null }))).toMatchObject({ ok: true });
    // The token's Environments were narrowed after the plan was presented: the step stops rather than run as the member.
    const id = yield* staged;
    yield* call("proposal_present", { proposal: id });
    yield* provided(Effect.flatMap(Database, ({ drizzle }) => drizzle.update(operationProposals).set({ callerEnvironments: ["staging"] }).where(eq(operationProposals.id, id))));
    expect(yield* cluster(decideProposal(owner, id, approve))).toMatchObject({ ok: false, message: expect.stringMatching(/^It didn't run: .*staging.* It's still waiting for approval\.$/) });
    expect(yield* acts).toEqual([]);
  }));

it.live("the sweep starts a step whose kickoff was lost, once, and never a step whose turn hasn't come", () =>
  Effect.gen(function* () {
    const { call, staged, acts, cluster, provided, owner, ended } = yield* shop();
    const id = yield* staged;
    yield* call("proposal_present", { proposal: id });
    // Approved in the database but never kicked off: Cloud fell over between the two.
    yield* provided(Effect.flatMap(Database, ({ drizzle }) => drizzle.update(operationProposals)
      .set({ status: "approved", decidedByUserId: owner.userId, decidedAt: new Date() }).where(eq(operationProposals.id, id))));
    expect(yield* cluster(sweepProposals())).toEqual([id]);
    let rows = yield* acts;
    expect(rows).toMatchObject([{ verb: "stop", step: 0 }]);
    // Step 0 still running: another sweep starts nothing. Done and its advance lost: the sweep starts step 1, once.
    yield* cluster(sweepProposals());
    expect(yield* acts).toHaveLength(1);
    yield* ended(rows[0]?.id ?? expect.fail("no act"), "done");
    yield* cluster(sweepProposals());
    yield* cluster(sweepProposals());
    rows = yield* acts;
    expect(rows).toMatchObject([{ verb: "stop", step: 0 }, { verb: "start", step: 1 }]);
    // A plan approved over an hour ago is past every step's window and is left alone.
    yield* provided(Effect.flatMap(Database, ({ drizzle }) => drizzle.update(operationProposals).set({ decidedAt: new Date(Date.now() - 2 * 60 * 60_000) }).where(eq(operationProposals.id, id))));
    expect(yield* cluster(sweepProposals())).toEqual([]);
  }));

it.live("a denial with a blank reason is a denial with none", () =>
  Effect.gen(function* () {
    const { call, staged, cluster, owner } = yield* shop();
    const id = yield* staged;
    yield* call("proposal_present", { proposal: id });
    expect(yield* cluster(decideProposal(owner, id, { reject: { reason: "  " } }))).toMatchObject({ ok: true, proposal: { status: "denied", reason: null } });
  }));

/** The tab the agent's session stands for, signed in, and a Server the Store can deploy to. */
const deployable = Effect.fn(function* () {
  const shopped = yield* shop();
  shopped.enforce(false);
  yield* shopped.provided(enrollStoreServer(ORGANIZATION, { secret: "ppair_fixture_store", server: MACHINE as MachineId }));
  yield* shopped.provided(Effect.flatMap(Database, ({ drizzle }) => drizzle.insert(session).values({
    id: BROWSER, userId: shopped.agent.userId, token: "owner-browser-token", expiresAt: new Date(Date.now() + 86_400_000),
  })));
  return shopped;
});

const Linked = Schema.Struct({ ok: Schema.Literal(true), value: Schema.Struct({ proposal_id: Schema.String, status: Schema.Literal("presented"), url: Schema.String }) });

/** The agent's `deploy`, answered with the one link a person opens. */
const linkOf = (answer: ToolOutcome) =>
  Schema.decodeUnknownOption(Linked)(answer).pipe(Option.map(({ value }) => value), Option.getOrElse(() => expect.fail(`no link: ${JSON.stringify(answer)}`)));

it.live("an agent's Deploy becomes one proposal with one link, and one person's approval admits it once", () =>
  Effect.gen(function* () {
    const { call, cluster, provided, owner, approve, admitted } = yield* deployable();
    const { proposal_id: id, url } = linkOf(yield* call("deploy", {}));
    expect(url).toBe(`http://localhost:3000/cloud/shop/approve/${id}`);
    expect(yield* provided(pendingProposals(ORGANIZATION))).toMatchObject([{ id, status: "presented", steps: [{ verb: "deploy" }] }]);
    expect(yield* admitted(id)).toBe(0);
    expect(yield* cluster(decideProposal(owner, id, approve))).toMatchObject({ ok: true, proposal: { status: "approved" } });
    expect(yield* admitted(id)).toBe(1);
    // A second answer, and the sweep that runs the step again, admit nothing more.
    expect(yield* cluster(decideProposal(owner, id, approve))).toMatchObject({ ok: false, message: expect.stringContaining("already decided") });
    yield* cluster(sweepProposals());
    expect(yield* admitted(id)).toBe(1);
    // The proposal reads as its Deployment: not done until it lands.
    expect(yield* provided(proposalStatus(owner, id))).toMatchObject({ ok: true, value: { steps: [{ state: expect.stringMatching(/^(accepted|running)$/) }] } });
    yield* provided(Effect.flatMap(Database, ({ drizzle }) => drizzle.execute(sql`update config_deployment set status = 'applied' where organization_id = ${ORGANIZATION} and id <> ${DEPLOYMENT}`)));
    expect(yield* provided(proposalStatus(owner, id))).toMatchObject({ ok: true, value: { steps: [{ state: "done" }] } });
  }));

it.live("an Environment edited after the link was made is denied at the tap and admits nothing", () =>
  Effect.gen(function* () {
    const { call, cluster, provided, owner, approve, write, admitted } = yield* deployable();
    const { proposal_id: id } = linkOf(yield* call("deploy", {}));
    yield* write({ command: "edit", environment: production, expect: null, changes: [{ op: "set", path: "web.replicas", value: 2 }] });
    expect(yield* cluster(decideProposal(owner, id, approve))).toMatchObject({ ok: false, message: DEPLOY_MOVED });
    // The one who asked said yes: a plan that moved under them is denied, not withdrawn.
    expect(yield* provided(proposalStatus(owner, id))).toMatchObject({ ok: true, value: { status: "denied", withdrawn: false } });
    expect(yield* admitted(id)).toBe(0);
  }));

it.live("a session that signed out before the tap leaves the Deploy unrun, though its owner is still a member", () =>
  Effect.gen(function* () {
    const { call, cluster, provided, owner, approve, admitted } = yield* deployable();
    const { proposal_id: id } = linkOf(yield* call("deploy", {}));
    yield* provided(Effect.flatMap(Database, ({ drizzle }) => drizzle.delete(session).where(eq(session.id, BROWSER))));
    expect(yield* cluster(decideProposal(owner, id, approve))).toMatchObject({ ok: false, message: expect.stringContaining("is gone") });
    expect(yield* admitted(id)).toBe(0);
    expect(yield* provided(proposalStatus(owner, id))).toMatchObject({ ok: true, value: { status: "denied", withdrawn: false } });
  }));

it.live("a Deploy asked with no_prune is admitted with no_prune, not as a Deploy that removes what else runs", () =>
  Effect.gen(function* () {
    const { call, cluster, owner, approve, store } = yield* deployable();
    const { proposal_id: id } = linkOf(yield* call("deploy", { no_prune: true }));
    const writes = vi.spyOn(store, "write");
    expect(yield* cluster(decideProposal(owner, id, approve))).toMatchObject({ ok: true });
    expect(writes.mock.calls.map(([, command]) => command)).toMatchObject([{ command: "admit", admit: "deploy", no_prune: true }]);
  }));

const linkedId = (link: DeployLink) => link !== null && "linked" in link ? link.linked.proposal_id : expect.fail(`no link: ${JSON.stringify(link)}`);

/** The Deploy step proposal `id` holds now. */
const deployStep = (id: string) => Effect.gen(function* () {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ status: operationProposals.status, steps: operationProposals.steps }).from(operationProposals).where(eq(operationProposals.id, id));
  const [step] = row?.steps ?? [];
  return step !== undefined && "deploy" in step ? { status: row?.status, ...step.deploy } : expect.fail("not a deploy");
});

/** `cache`, an image Service with a Volume: a Server that enforces device approval runs it only signed. */
const withCache = ({ write }: { readonly write: (command: ConfigCommand) => Effect.Effect<unknown> }) => Effect.gen(function* () {
  const here = { project: null, environment: null };
  yield* write({ command: "create_service", id: "00000000-0000-4000-8000-00000000b3c0", environment: here, name: "cache", image: "redis:7" });
  yield* write({ command: "create_volume", id: "00000000-0000-4000-8000-00000000b3c1", environment: here, name: "data", storage: { kind: "docker" }, mounts: [{ service: "cache", path: "/data" }] });
});

/** A member who can't protect, signed in on their own browser, asking through an agent. */
const memberAgent = Effect.gen(function* () {
  const { drizzle } = yield* Database;
  const [person] = yield* drizzle.insert(user).values({ email: "ada.member@example.test", name: "Ada" }).returning();
  const userId = person?.id ?? expect.fail("no user");
  yield* drizzle.insert(member).values({ userId, organizationId: ORGANIZATION, role: "member" });
  yield* drizzle.insert(session).values({ id: ADA_BROWSER, userId, token: "ada-browser-token", expiresAt: new Date(Date.now() + 86_400_000) });
  return { userId, organization: { id: ORGANIZATION, slug: "shop" }, credential: { kind: "session", id: ADA_BROWSER }, role: "member", support: null, via: "agent" } satisfies Caller;
});

const madePassword: Change[] = [{ op: "set", path: envPath("web", "PASSWORD"), value: "made" }];

/** An approval page's fill of `changes`, sent at `seen`, the version the page loaded: by default the one the proposal holds now. */
const filledBy = (decider: Caller, id: string, seen?: string | null, changes = madePassword) => Effect.gen(function* () {
  return yield* fillDeploy(decider, id, seen === undefined ? (yield* deployStep(id)).command.version ?? null : seen, changes);
});

/** The revision a review version names. */
const revisionOf = (version: string | null | undefined) => Number(/^(\d+):/u.exec(version ?? "")?.[1]);

/**
 * The Store as it answers a copy whose password has no value yet: it refuses to plan until an edit sets it. With `loss`,
 * a Deploy then deletes the data in `data`, which it asks to accept by name and a person to approve, and admits once both
 * are redeemed. Every other answer is the Store's own.
 */
const pendingCopy = (store: ConfigStore, diff: DiffView) => {
  const answers = { pending: true, loss: false };
  const read = store.read.bind(store);
  const write = store.write.bind(store);
  vi.spyOn(store, "read").mockImplementation((organization: string, query: ConfigQuery, trusted?: ConfigTrusted) => {
    if (!answers.pending || query.query !== "plan") return read(organization, query, trusted);
    return Promise.reject(new RpcError({ code: "invalid_argument", message: "web.PASSWORD has no value yet.", details: {
      variable: "web.PASSWORD", environment: ENVIRONMENT, pending: [{ service: "web", id: SERVICE, key: "PASSWORD", generated: "password" }],
    } }));
  });
  vi.spyOn(store, "write").mockImplementation(async (organization, command, trusted, principal) => {
    if (command.command === "edit") {
      const written = await write(organization, command, trusted, principal);
      if (command.changes.some((change) => "path" in change && change.path === madePassword[0]?.path)) answers.pending = false;
      return written;
    }
    if (!answers.loss || command.command !== "admit" || command.admit !== "deploy") return write(organization, command, trusted, principal);
    const [version = ""] = (command.version ?? "").split("#loss-");
    const approval = `${version}:loss`;
    if (!(command.accept_volume_loss ?? []).includes("data")) {
      throw new RpcError({ code: "confirmation_required", message: "Deploying loses data of data. Accept each by name", details: { accept: ["data"], version: `${version}#loss-0` } });
    }
    if (trusted?.approval === "required" || JSON.stringify(trusted?.approval) !== JSON.stringify({ approved: approval })) {
      throw new RpcError({ code: "approval_required", message: "A human must approve this first: deletes data", details: {
        approval, effects: [{ kind: "deletes_volume", name: "data", node: "data", path: "data" }], diff: { ...diff, version },
      } });
    }
    return write(organization, { ...command, version: null, accept_volume_loss: [] }, trusted && { ...trusted, approval: "not_required" }, principal);
  });
  return answers;
};

it.live("a keyless CLI Deploy queues as before when Cloud can't read every Server; an agent's is refused", () =>
  Effect.gen(function* () {
    const shopped = yield* deployable();
    const { provided, agent } = shopped;
    yield* withCache(shopped);
    // No Server answers Cloud, so nothing says whether one would refuse `cache` unsigned. Each Server still checks it.
    const command = admitDeploy(production, ASKED);
    expect(yield* provided(linkDeploy({ ...agent, via: "cli" }, command))).toBeNull();
    expect(yield* provided(linkDeploy(agent, command))).toMatchObject({ refused: { message: expect.stringContaining("Couldn't reach your servers") } });
    expect(yield* provided(pendingProposals(ORGANIZATION))).toEqual([]);
  }));

it.live("a full Deploy's prune is on the approval page, and a Service running there since is refused, not pruned", () =>
  Effect.gen(function* () {
    const { call, cluster, provided, owner, approve, admitted, runs } = yield* deployable();
    runs(["old"]);
    const { proposal_id: id } = linkOf(yield* call("deploy", {}));
    expect(yield* provided(deployStep(id))).toMatchObject({ review: { review: { effects: [{ kind: "removes_service", name: "old" }] } } });
    runs(["old", "other"]);
    expect(yield* cluster(decideProposal(owner, id, approve))).toMatchObject({ ok: false });
    expect(yield* admitted(id)).toBe(0);
    expect(yield* provided(deployStep(id))).toMatchObject({ status: "denied" });
  }));

it.live("a member's Deploy to a Protected Environment that only the page can finish gets the link, and only someone who can protect it approves", () =>
  Effect.gen(function* () {
    const { cluster, provided, owner, approve, admitted, protect, store } = yield* deployable();
    yield* protect;
    pendingCopy(store, yield* provided(readStore(ORGANIZATION, { query: "diff", environment: production })));
    const ada = yield* provided(memberAgent);
    const id = linkedId(yield* cluster(linkDeploy(ada, admitDeploy(production, ASKED))));
    expect(yield* cluster(filledBy(owner, id))).toMatchObject({ ok: true });
    expect(yield* provided(deployStep(id))).toMatchObject({ review: { review: { protected: { environment: "production" } } } });
    expect(yield* cluster(decideProposal(ada, id, approve))).toMatchObject({ ok: false, message: expect.stringContaining("Only an owner or admin") });
    expect(yield* admitted(id)).toBe(0);
    expect(yield* cluster(decideProposal(owner, id, approve))).toMatchObject({ ok: true, proposal: { status: "approved" } });
    expect(yield* admitted(id)).toBe(1);
  }));

// Catches a protected member's signed rule riding their proposal, so a protector's yes to the Deploy turns on every push.
it.live("a member's CLI Deploy to a Protected Environment drops the automatic deploy rule it signed, and its approval keeps none", () =>
  Effect.gen(function* () {
    const { cluster, provided, owner, approve, admitted, protect, store } = yield* deployable();
    yield* protect;
    pendingCopy(store, yield* provided(readStore(ORGANIZATION, { query: "diff", environment: production })));
    const ada = { ...(yield* provided(memberAgent)), via: "cli" } satisfies Caller;
    const rule = asTestDouble<SavedRule>()({ rule: { body: "{}", signatures: [] }, births: {}, prior: null });
    const id = linkedId(yield* cluster(linkDeploy(ada, { ...admitDeploy(production, ASKED), rule })));
    expect(yield* cluster(filledBy(owner, id))).toMatchObject({ ok: true });
    expect((yield* provided(deployStep(id))).command).not.toHaveProperty("rule");
    expect(yield* cluster(decideProposal(owner, id, approve))).toMatchObject({ ok: true, proposal: { status: "approved" } });
    expect(yield* admitted(id)).toBe(1);
    expect((yield* provided(deployStep(id))).command).not.toHaveProperty("rule");
  }));

it.live("a member's dashboard Deploy to a Protected Environment needs no passkey of theirs, so it reaches the proposal", () =>
  Effect.gen(function* () {
    const shopped = yield* deployable();
    const { cluster, provided, protect, enforce, write } = shopped;
    yield* withCache(shopped);
    enforce(true);
    const ada = yield* provided(memberAgent);
    const plan = cluster(prepareDeployTap({ userId: ada.userId }, { organizationSlug: "shop", environment: production, services: [] }));
    // Unprotected, the member signs it, so the Servers' gate applies to them.
    yield* write({ command: "set_environment_protected", environment: { project: null, environment: null }, protected: false });
    expect(yield* plan).toMatchObject({ kind: "plan", tap: { kind: "refused" } });
    yield* protect;
    expect(yield* plan).toMatchObject({ kind: "plan", tap: { kind: "off" } });
  }));

it.live("the same Deploy asked twice at once is one proposal, and its Deployment id asked with another body is refused", () =>
  Effect.gen(function* () {
    const { cluster, provided, agent } = yield* deployable();
    const command = admitDeploy(production, ASKED);
    const [first, second] = yield* Effect.all([cluster(linkDeploy(agent, command)), cluster(linkDeploy(agent, command))], { concurrency: 2 });
    expect(linkedId(second)).toBe(linkedId(first));
    expect(yield* provided(pendingProposals(ORGANIZATION))).toHaveLength(1);
    expect(yield* cluster(linkDeploy(agent, { ...command, no_prune: true }))).toMatchObject({ refused: { code: "conflict" } });
    expect(yield* provided(pendingProposals(ORGANIZATION))).toHaveLength(1);
  }));

it.live("a copy whose password has no value yet and whose Deploy loses data takes one fill and one tap, the loss bound first", () =>
  Effect.gen(function* () {
    const { cluster, provided, owner, agent, approve, admitted, store } = yield* deployable();
    const answers = pendingCopy(store, yield* provided(readStore(ORGANIZATION, { query: "diff", environment: production })));
    answers.loss = true;
    const id = linkedId(yield* cluster(linkDeploy(agent, admitDeploy(production, ASKED))));
    expect((yield* provided(deployStep(id))).review).toBeUndefined();
    expect(yield* cluster(filledBy(owner, id))).toMatchObject({ ok: true });
    expect(yield* provided(deployStep(id))).toMatchObject({
      command: { accept_volume_loss: ["data"], version: expect.stringMatching(/#loss-0$/) },
      review: { review: { effects: [{ kind: "deletes_volume", name: "data" }] } },
    });
    expect(yield* cluster(decideProposal(owner, id, approve))).toMatchObject({ ok: true, proposal: { status: "approved" } });
    expect(yield* admitted(id)).toBe(1);
  }));

it.live("a fill another edit beat denies the proposal, even an edit that made the same password", () =>
  Effect.gen(function* () {
    const { cluster, provided, owner, agent, store, write } = yield* deployable();
    const answers = pendingCopy(store, yield* provided(readStore(ORGANIZATION, { query: "diff", environment: production })));

    const id = linkedId(yield* cluster(linkDeploy(agent, admitDeploy(production, ASKED))));
    // One edit made the password and changed the replicas. No fill of this proposal wrote it.
    yield* write({ command: "edit", environment: production, expect: revisionOf((yield* provided(deployStep(id))).command.version), changes: [...madePassword, { op: "set", path: "web.replicas", value: 2 }] });
    expect(yield* cluster(filledBy(owner, id))).toMatchObject({ ok: false, message: DEPLOY_MOVED });
    expect(yield* provided(deployStep(id))).toMatchObject({ status: "denied" });

    answers.pending = true;
    const stale = linkedId(yield* cluster(linkDeploy(agent, admitDeploy(production, ASKED_AGAIN))));
    yield* write({ command: "edit", environment: production, expect: null, changes: [{ op: "set", path: "web.replicas", value: 3 }] });
    expect(yield* cluster(filledBy(owner, stale))).toMatchObject({ ok: false, message: DEPLOY_MOVED });
    expect(yield* provided(deployStep(stale))).toMatchObject({ status: "denied" });
  }));

it.live("a second approval page that loaded before the first page's fill resumes that fill and makes no password of its own", () =>
  Effect.gen(function* () {
    const { cluster, provided, owner, agent, approve, admitted, store } = yield* deployable();
    pendingCopy(store, yield* provided(readStore(ORGANIZATION, { query: "diff", environment: production })));
    const id = linkedId(yield* cluster(linkDeploy(agent, admitDeploy(production, ASKED))));
    const loaded = (yield* provided(deployStep(id))).command.version ?? null;
    const first = yield* cluster(filledBy(owner, id, loaded));
    expect(first).toMatchObject({ ok: true });
    const bound = (yield* provided(deployStep(id))).command.version;
    expect(yield* cluster(filledBy(owner, id, loaded, [{ op: "set", path: envPath("web", "PASSWORD"), value: "made again" }]))).toEqual(first);
    expect((yield* provided(deployStep(id))).command.version).toBe(bound);
    expect(revisionOf((yield* provided(readStore(ORGANIZATION, { query: "diff", environment: production }))).version)).toBe(revisionOf(bound));
    expect(yield* cluster(decideProposal(owner, id, approve))).toMatchObject({ ok: true, proposal: { status: "approved" } });
    expect(yield* admitted(id)).toBe(1);
  }));

it.live("the same Deploy asked again after its approval finds that proposal; another body under its id is refused", () =>
  Effect.gen(function* () {
    const { cluster, owner, agent, approve, admitted } = yield* deployable();
    const command = admitDeploy(production, ASKED);
    const id = linkedId(yield* cluster(linkDeploy(agent, command)));
    expect(yield* cluster(decideProposal(owner, id, approve))).toMatchObject({ ok: true, proposal: { status: "approved" } });
    expect(yield* cluster(linkDeploy(agent, command))).toMatchObject({ linked: { proposal_id: id, status: "approved" } });
    expect(yield* cluster(linkDeploy(agent, { ...command, no_prune: true }))).toMatchObject({ refused: { code: "conflict" } });
    expect(yield* admitted(id)).toBe(1);
  }));

it.live("a full Deploy whose only reviewed prune target is gone by the tap is refused, not run", () =>
  Effect.gen(function* () {
    const { call, cluster, provided, owner, approve, admitted, runs } = yield* deployable();
    runs(["old"]);
    const { proposal_id: id } = linkOf(yield* call("deploy", {}));
    expect(yield* provided(deployStep(id))).toMatchObject({ review: { review: { effects: [{ kind: "removes_service", name: "old" }] } } });
    runs([]);
    expect(yield* cluster(decideProposal(owner, id, approve))).toMatchObject({ ok: false });
    expect(yield* admitted(id)).toBe(0);
  }));

it.live("a CLI that signs with its device key gets no link for a Pending value or a Volume loss", () =>
  Effect.gen(function* () {
    const { cluster, provided, agent, store } = yield* deployable();
    const answers = pendingCopy(store, yield* provided(readStore(ORGANIZATION, { query: "diff", environment: production })));
    const device: Caller = { ...agent, via: "cli" };
    const signed = { ...admitDeploy(production, ASKED), versions: [{ statement: { body: JSON.stringify({ service: "web" }), signatures: [] } }] };
    expect(yield* cluster(linkDeploy(device, signed))).toBeNull();
    answers.pending = false;
    answers.loss = true;
    expect(yield* cluster(linkDeploy(device, signed))).toBeNull();
    expect(yield* provided(pendingProposals(ORGANIZATION))).toEqual([]);
  }));

it.live("a Deploy a Server on Ployz 0.3 would refuse unsigned gets no link, and nothing waits for approval", () =>
  Effect.gen(function* () {
    const shopped = yield* deployable();
    const { cluster, provided, agent } = shopped;
    yield* withCache(shopped);
    expect(yield* cluster(linkDeploy(agent, admitDeploy(production, ASKED)))).toMatchObject({
      refused: { code: "conflict", message: expect.stringContaining("Upgrade machine-2 first.") },
    });
    expect(yield* provided(pendingProposals(ORGANIZATION))).toEqual([]);
  }));

it.live("an agent token revoked before the tap gets nothing run, though its owner is still a member", () =>
  Effect.gen(function* () {
    const { cluster, provided, owner, approve, admitted } = yield* deployable();
    const minted = yield* provided(createOrganizationToken({ ...owner, via: "agent" }, { name: "agent", expiresInDays: 1 }));
    const id = linkedId(yield* cluster(linkDeploy({ ...owner, via: "agent", credential: { kind: "token", id: minted.id, grant: FULL_GRANT } }, admitDeploy(production, ASKED))));
    yield* provided(revokeCredential(owner, minted.id));
    expect(yield* cluster(decideProposal(owner, id, approve))).toMatchObject({ ok: false, message: expect.stringContaining("is gone") });
    expect(yield* admitted(id)).toBe(0);
    expect(yield* provided(deployStep(id))).toMatchObject({ status: "denied" });
  }));

it.live("the CLI follows a Deploy's proposal only through the credential that asked, and reads only its status", () =>
  Effect.gen(function* () {
    const { cluster, provided, agent } = yield* deployable();
    const asker: CliCall["caller"] = { userId: agent.userId, organization: agent.organization, role: "owner", support: null, via: "agent", credential: { kind: "session", id: BROWSER } };
    const id = linkedId(yield* cluster(linkDeploy(asker, admitDeploy(production, ASKED))));
    const read = (caller: CliCall["caller"]) => provided(proposalsCliRoutes["GET proposals/:id"]({ caller, id, request: new Request("http://cloud.test") }));
    expect(yield* read(asker)).toEqual({ proposal: { status: "presented", reason: null, deployment_id: ASKED } });
    // The same person's staging token can't read the production proposal its maker's sign-in asked for.
    const staging: CliCall["caller"] = { ...asker, via: "cli", credential: { kind: "token", id: "token-staging", grant: { permissions: null, environments: ["staging"] } } };
    const refused = yield* read(staging);
    if (!(refused instanceof Response)) return expect.fail(`read: ${JSON.stringify(refused)}`);
    expect(yield* Effect.promise(() => refused.json())).toMatchObject({ error: { code: "not_found" } });
  }));
