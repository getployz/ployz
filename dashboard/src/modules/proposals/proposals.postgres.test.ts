import { it } from "@effect/vitest";
import type { RuntimeWatchView } from "@ployz/sdk";
import { eq, sql } from "drizzle-orm";
import { Effect, Option, Schema } from "effect";
import { expect } from "vitest";
import type { JsonObject } from "#/db/tables";
import { asTestDouble } from "#/lib/test-double";
import { type AgentCaller, type Asking, runAgentCall } from "#/modules/agent/agent-runner.server";
import type { ToolOutcome } from "#/modules/agent/agent";
import { findService } from "#/modules/config-store/store-lookup.server";
import type { Caller } from "#/modules/identity/actor";
import { member, user } from "#/modules/identity/tables";
import { PROPOSAL_CAPABILITY, type ProposalApprovals } from "#/modules/proposals/proposal";
import {
  advanceProposal,
  createProposal,
  decideProposal,
  pendingProposals,
  presentProposal,
  proposalStatus,
  readProposalTarget,
  stageStep,
  sweepProposals,
} from "#/modules/proposals/proposals.server";
import { operationProposals } from "#/modules/proposals/tables";
import { OrganizationRuntime, type OrganizationRuntimeService } from "#/modules/runtime/organization-runtime.server";
import type { PloyzSession } from "#/modules/runtime/ployz.server";
import { APPROVAL_RAN_OUT, type ServiceActLeg, START_APPROVAL_CAPABILITY } from "#/modules/service-act/service-act";
import { serviceAct } from "#/modules/service-act/tables";
import { AuthLive } from "#/server/auth.server";
import { Database } from "#/server/database.server";
import { acmeWeb, seedStoreGitService, seedStoreOrganization, storeTestCloudWith } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000b301";
const PROJECT = "00000000-0000-4000-8000-00000000b303";
const ENVIRONMENT = "00000000-0000-4000-8000-00000000b304";
const SERVICE = "00000000-0000-4000-8000-00000000b305";
const DEPLOYMENT = "00000000-0000-4000-8000-00000000b306";
const CONTAINER = "c".repeat(64);
const MACHINE = "b".repeat(32);
const production = { project: "shop", environment: "production" };
const page = { at: "here", ...production } as const;

const Created = Schema.Struct({ ok: Schema.Literal(true), value: Schema.Struct({ proposal_id: Schema.String }) });
const idOf = (result: { readonly asking: Asking } | ToolOutcome) =>
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
  const agent: AgentCaller = { userId, organization: { id: ORGANIZATION, slug: "shop" }, credential: { kind: "session", id: "owner-browser" }, role: "owner", support: null, via: "agent" };
  const owner: Caller = { ...agent, via: "dashboard" };
  let capabilities = ["ployz.restart.session-grant.v2", START_APPROVAL_CAPABILITY, PROPOSAL_CAPABILITY];
  let readable = true;
  const frame = () => asTestDouble<RuntimeWatchView>()({
    services: [{ identity: `${found.namespace}/${found.runtime}`, containers: [{ container_id: CONTAINER, machine_id: MACHINE }], hook_containers: [] }],
    machines: [{ machine: { id: MACHINE, name: "machine-2", runtime: { device_enforcement: true, capabilities } } }],
    containers: [],
  });
  const cluster = <A, E, R>(effect: Effect.Effect<A, E, R>) => provided(effect.pipe(Effect.provideService(
    OrganizationRuntime,
    asTestDouble<OrganizationRuntimeService>()({
      open: () => readable
        ? Effect.succeed({ status: "connected" as const, connected: asTestDouble<PloyzSession>()({ watchFirstFrame: () => Effect.succeed(frame()) }) })
        : Effect.succeed({ status: "unreachable" as const }),
    }),
  )));
  const acts = provided(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    return yield* drizzle.select({ id: serviceAct.id, verb: serviceAct.verb, step: serviceAct.proposalStep, proposal: serviceAct.proposalId, approvals: serviceAct.approvals, key: serviceAct.state })
      .from(serviceAct).orderBy(serviceAct.requestedAt);
  }));
  const ended = (actId: string, state: "done" | "failed") => provided(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* drizzle.update(serviceAct).set({ state, endedAt: new Date() }).where(eq(serviceAct.id, actId));
  }));
  const call = (tool: string, input: JsonObject, key = `call-${Math.random()}`) =>
    cluster(runAgentCall(agent, { tool, input, key, approvalId: null, page })).pipe(Effect.map((result) => "asking" in result ? result : result.outcome));
  const staged = Effect.gen(function* () {
    const id = idOf(yield* call("proposal_create", {}));
    expect(yield* call("service_stop", { service: "web", proposal: id })).toMatchObject({ ok: true, value: { step: 1 } });
    expect(yield* call("service_start", { service: "web", proposal: id })).toMatchObject({ ok: true, value: { step: 2 } });
    return id;
  });
  const bundle = { body: JSON.stringify({ expires_at: Date.now() + 15 * 60_000 }), signatures: [{ key: "d".repeat(64), signature: "e".repeat(128) }] };
  const envelope = (step: number) => ({ body: `{"step":${step}}`, signatures: [], proposal: bundle });
  const approvals: ProposalApprovals = { "0": { [CONTAINER]: envelope(0) }, "1": { [CONTAINER]: envelope(1) } };
  const approve = { approve: { approvals, bundle: envelope(0).proposal } };
  return { provided, cluster, agent, owner, found, acts, ended, call, staged, approvals, approve, protect, setCapabilities: (next: string[]) => { capabilities = next; }, setReadable: (next: boolean) => { readable = next; } };
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
    const other = yield* provided(createProposal({ ...agent, credential: { kind: "oauth", id: "cli-oauth" } }, production));
    const otherId = other.ok && "value" in other ? other.value.proposal_id : expect.fail("no proposal");
    expect(yield* provided(stageStep(agent, otherId, { environment: ENVIRONMENT, service: "web", verb: "stop", only: null }))).toMatchObject({ ok: false, refusal: { code: "forbidden" } });
    expect(yield* provided(presentProposal(agent, otherId))).toMatchObject({ ok: false, refusal: { code: "forbidden" } });
  }));

it.live("an approval runs the first step with its envelopes, each done step starts the next, and a failed one stops the rest", () =>
  Effect.gen(function* () {
    const { call, staged, acts, cluster, provided, owner, ended, approve, approvals } = yield* shop();
    const id = yield* staged;
    yield* call("proposal_present", { proposal: id });
    expect(yield* cluster(decideProposal(owner, id, approve))).toMatchObject({ ok: true, proposal: { status: "approved" } });
    let rows = yield* acts;
    expect(rows).toMatchObject([{ verb: "stop", step: 0, proposal: id, approvals: approvals["0"] }]);
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
    expect(rows).toMatchObject([{ verb: "stop", step: 0 }, { verb: "start", step: 1, proposal: id, approvals: approvals["1"] }]);
    const second = rows[1]?.id ?? expect.fail("no second act");
    yield* ended(second, "done");
    yield* cluster(advanceProposal({ actId: second, organizationId: ORGANIZATION }));
    expect(yield* acts).toHaveLength(2);
    expect(yield* provided(proposalStatus(owner, id))).toMatchObject({ ok: true, value: { steps: [{ state: "done" }, { state: "done" }] } });
  }));

it.live("the approver's browser gets each step's runtime name from the Store, never from the row", () =>
  Effect.gen(function* () {
    const { call, staged, cluster, provided, found } = yield* shop();
    const id = yield* staged;
    yield* call("proposal_present", { proposal: id });
    // A row edited to send the stop to another Service's Containers: the browser reads what the Store names for `web`.
    yield* provided(Effect.flatMap(Database, ({ drizzle }) => drizzle.execute(sql`update operation_proposals set steps = jsonb_set(steps, '{0,runtime}', '"api"') where id = ${id}`)));
    expect(yield* cluster(readProposalTarget(ORGANIZATION, id))).toMatchObject({
      project: "shop", environmentName: "production", namespace: found.namespace,
      steps: [{ verb: "stop", service: "web", runtime: "web", copies: [{ id: CONTAINER, machineId: MACHINE }] }, { verb: "start", service: "web", runtime: "web" }],
    });
    // A step naming a Service the Store doesn't have gets no runtime name and no Containers.
    yield* provided(Effect.flatMap(Database, ({ drizzle }) => drizzle.execute(sql`update operation_proposals set steps = jsonb_set(steps, '{0,service}', '"api"') where id = ${id}`)));
    expect(yield* cluster(readProposalTarget(ORGANIZATION, id))).toMatchObject({ steps: [{ service: "api", runtime: null, copies: [] }, { service: "web", runtime: "web" }] });
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

it.live("an approval is refused, running nothing, when it doesn't cover every step, Cloud can't read the Servers, or a Server can't take it", () =>
  Effect.gen(function* () {
    const { call, staged, acts, cluster, provided, owner, approve, approvals, setCapabilities, setReadable } = yield* shop();
    const id = yield* staged;
    yield* call("proposal_present", { proposal: id });
    const refusal = (decision: Parameters<typeof decideProposal>[2]) => cluster(decideProposal(owner, id, decision)).pipe(Effect.map((decided) => decided.ok ? expect.fail("approved") : decided.message));
    expect(yield* refusal({ approve: { approvals: { "0": approvals["0"] ?? {} }, bundle: approve.approve.bundle } })).toBe("The approval doesn't cover every step. Nothing was approved.");
    setReadable(false);
    expect(yield* refusal(approve)).toBe("Cloud couldn't check the servers. Try again.");
    setReadable(true);
    setCapabilities(["ployz.restart.session-grant.v2", START_APPROVAL_CAPABILITY]);
    expect(yield* refusal(approve)).toBe("machine-2 needs a newer Ployz to stop it.");
    setCapabilities(["ployz.restart.session-grant.v2", PROPOSAL_CAPABILITY]);
    expect(yield* refusal(approve)).toBe("machine-2 needs a newer Ployz to stop it.");
    expect(yield* acts).toEqual([]);
    expect(yield* provided(pendingProposals(ORGANIZATION))).toMatchObject([{ id, status: "presented" }]);
    // A step swapped in the database after the person saw the card is what they didn't sign: the Server refuses it by its digest. Cloud only ever runs what the row holds.
    setCapabilities(["ployz.restart.session-grant.v2", START_APPROVAL_CAPABILITY, PROPOSAL_CAPABILITY]);
    const denied = yield* cluster(decideProposal(owner, id, { reject: { reason: "not during the sale" } }));
    expect(denied).toMatchObject({ ok: true, proposal: { status: "denied", reason: "not during the sale" } });
    expect(yield* call("proposal_status", { proposal: id })).toMatchObject({ ok: true, value: { status: "denied", reason: "not during the sale" } });
    expect(yield* provided(pendingProposals(ORGANIZATION))).toEqual([]);
  }));

it.live("a first step that can't start hands the proposal back to waiting", () =>
  Effect.gen(function* () {
    const { call, staged, acts, cluster, provided, owner, approve, agent } = yield* shop();
    const id = yield* staged;
    yield* call("proposal_present", { proposal: id });
    yield* provided(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      yield* drizzle.delete(member).where(eq(member.userId, agent.userId));
    }));
    expect(yield* cluster(decideProposal(owner, id, approve))).toMatchObject({ ok: false, message: expect.stringMatching(/^It didn't run: .* It's still waiting for approval\.$/) });
    expect(yield* acts).toEqual([]);
    expect(yield* provided(pendingProposals(ORGANIZATION))).toMatchObject([{ id, status: "presented" }]);
    const [row] = yield* provided(Effect.flatMap(Database, ({ drizzle }) => drizzle.select({ bundle: operationProposals.bundle, halt: operationProposals.halt }).from(operationProposals).where(eq(operationProposals.id, id))));
    expect(row).toEqual({ bundle: null, halt: null });
  }));

it.live("a proposal in a Protected Environment is presented only by someone who could protect it", () =>
  Effect.gen(function* () {
    const { provided, call, agent, protect } = yield* shop();
    const id = idOf(yield* call("proposal_create", {}));
    const [person] = yield* provided(Effect.flatMap(Database, ({ drizzle }) => drizzle.insert(user).values({ email: "ada.member@example.test", name: "Ada Member" }).returning()));
    yield* provided(Effect.flatMap(Database, ({ drizzle }) => drizzle.insert(member).values({ userId: person?.id ?? "", organizationId: ORGANIZATION, role: "member" })));
    const ada: AgentCaller = { ...agent, userId: person?.id ?? "", credential: { kind: "session", id: "ada-session" }, role: "member" };
    const adas = yield* provided(createProposal(ada, production));
    const adaId = adas.ok && "value" in adas ? adas.value.proposal_id : expect.fail("no proposal");
    expect(yield* provided(stageStep(ada, adaId, { environment: ENVIRONMENT, service: "web", verb: "stop", only: null }))).toMatchObject({ ok: true });
    yield* protect;
    expect(yield* provided(presentProposal(ada, adaId))).toMatchObject({ ok: false, refusal: { code: "forbidden" } });
    expect(yield* provided(stageStep(agent, id, { environment: ENVIRONMENT, service: "web", verb: "stop", only: null }))).toMatchObject({ ok: true });
    expect(yield* provided(presentProposal(agent, id))).toMatchObject({ ok: true });
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

it.live("a step due after the approval's 15 minutes doesn't run, and the card says which", () =>
  Effect.gen(function* () {
    const { call, staged, acts, cluster, provided, owner, ended, approve } = yield* shop();
    const id = yield* staged;
    yield* call("proposal_present", { proposal: id });
    expect(yield* cluster(decideProposal(owner, id, approve))).toMatchObject({ ok: true });
    const [first] = yield* acts;
    yield* ended(first?.id ?? expect.fail("no act"), "done");
    yield* provided(Effect.flatMap(Database, ({ drizzle }) => drizzle.update(operationProposals)
      .set({ bundle: { ...approve.approve.bundle, body: JSON.stringify({ expires_at: Date.now() - 1 }) } }).where(eq(operationProposals.id, id))));
    yield* cluster(advanceProposal({ actId: first?.id ?? "", organizationId: ORGANIZATION }));
    expect(yield* acts).toHaveLength(1);
    expect(yield* provided(proposalStatus(owner, id))).toMatchObject({ ok: true, value: { steps: [
      { state: "done" },
      { state: "failed", reason: "The approval ran out. Start web yourself." },
    ] } });
  }));

it.live("a step a Server refused for the window reads as the approval running out, naming what to start", () =>
  Effect.gen(function* () {
    const { call, acts, cluster, provided, owner, ended, approve } = yield* shop();
    const id = idOf(yield* call("proposal_create", {}));
    yield* call("service_stop", { service: "web", proposal: id });
    yield* call("service_restart", { service: "web", proposal: id });
    yield* call("proposal_present", { proposal: id });
    expect(yield* cluster(decideProposal(owner, id, approve))).toMatchObject({ ok: true });
    const [first] = yield* acts;
    yield* ended(first?.id ?? expect.fail("no act"), "done");
    yield* cluster(advanceProposal({ actId: first?.id ?? "", organizationId: ORGANIZATION }));
    const [, second] = yield* acts;
    // The restart's stop leg went inside the window and its start leg fell past it.
    const leg: ServiceActLeg = { server: "machine-2", machine: MACHINE, container: CONTAINER, hook: false, stop: "done", start: "failed", reason: APPROVAL_RAN_OUT };
    yield* provided(Effect.flatMap(Database, ({ drizzle }) => drizzle.update(serviceAct)
      .set({ state: "failed", reason: `Container 1 on machine-2 didn't come back. ${APPROVAL_RAN_OUT}`, legs: [leg], endedAt: new Date() }).where(eq(serviceAct.id, second?.id ?? ""))));
    expect(yield* provided(proposalStatus(owner, id))).toMatchObject({ ok: true, value: { steps: [
      { state: "done" },
      { state: "failed", reason: "The approval ran out. Start web yourself." },
    ] } });
  }));

it.live("the sweep starts a step whose kickoff was lost, once, and never a step whose turn hasn't come", () =>
  Effect.gen(function* () {
    const { call, staged, acts, cluster, provided, owner, ended, approve, approvals } = yield* shop();
    const id = yield* staged;
    yield* call("proposal_present", { proposal: id });
    // Approved in the database but never kicked off: Cloud fell over between the two.
    yield* provided(Effect.flatMap(Database, ({ drizzle }) => drizzle.update(operationProposals)
      .set({ status: "approved", bundle: approve.approve.bundle, approvals, decidedByUserId: owner.userId, decidedAt: new Date() }).where(eq(operationProposals.id, id))));
    expect(yield* cluster(sweepProposals(new Date()))).toEqual([id]);
    let rows = yield* acts;
    expect(rows).toMatchObject([{ verb: "stop", step: 0 }]);
    // Step 0 still running: another sweep starts nothing. Done and its advance lost: the sweep starts step 1, once.
    yield* cluster(sweepProposals(new Date()));
    expect(yield* acts).toHaveLength(1);
    yield* ended(rows[0]?.id ?? expect.fail("no act"), "done");
    yield* cluster(sweepProposals(new Date()));
    yield* cluster(sweepProposals(new Date()));
    rows = yield* acts;
    expect(rows).toMatchObject([{ verb: "stop", step: 0 }, { verb: "start", step: 1, approvals: approvals["1"] }]);
    // A plan approved over an hour ago is past every step's window and is left alone.
    yield* provided(Effect.flatMap(Database, ({ drizzle }) => drizzle.update(operationProposals).set({ decidedAt: new Date(Date.now() - 2 * 60 * 60_000) }).where(eq(operationProposals.id, id))));
    expect(yield* cluster(sweepProposals(new Date()))).toEqual([]);
  }));

it.live("a denial with a blank reason is a denial with none", () =>
  Effect.gen(function* () {
    const { call, staged, cluster, owner } = yield* shop();
    const id = yield* staged;
    yield* call("proposal_present", { proposal: id });
    expect(yield* cluster(decideProposal(owner, id, { reject: { reason: "  " } }))).toMatchObject({ ok: true, proposal: { status: "denied", reason: null } });
  }));
