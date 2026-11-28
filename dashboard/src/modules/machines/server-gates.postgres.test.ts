import type { Inngest } from "inngest";
import { Cause, Effect, Exit } from "effect";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { runCommand } from "#/modules/commands/commands.server";
import type { Actor, Caller } from "#/modules/identity/actor";
import { user } from "#/modules/identity/tables";
import { member, organization } from "#/modules/organization/tables";
import { InngestClient } from "#/modules/inngest/client";
import { enqueueMachineRemove, loadMachineDataLoss } from "#/modules/machines/machine-removal.server";
import { loadNamespaceDataLoss, removeNamespace } from "#/modules/machines/namespace-cleanup.server";
import { requestServerDrain } from "#/modules/machines/server-drain.server";
import { planKeyedServerPolicyChange, requestServerPolicyChange } from "#/modules/machines/server-policy.server";
import { requestServerWebTraffic } from "#/modules/machines/server-web-traffic.server";
import { mintMachineEnrollment } from "#/modules/machines/enrollment.server";
import { SupportStaff, supportStaffOf } from "#/modules/organization/access.server";
import { forgetServerFromDashboard } from "#/modules/operations/operations.server";
import { provideServerAccess } from "#/modules/machines/server-access.server";
import { requestServerUpgrade } from "#/modules/server-upgrade/server-upgrade.server";
import { OrganizationRuntime, type ConnectedRuntimeClient, type OrganizationRuntimeService } from "#/modules/runtime/organization-runtime.server";
import { deleteCopy } from "#/modules/volume-placement/volume-placement.deploy.server";
import { testConfigLayer } from "#/test/config-environment";
import { startPostgresTestHarness, type PostgresTestHarness } from "#/test/postgres";

const LAB = "00000000-0000-4000-8000-00000000c002";
const SLUG = "lab";
const MISSING = "00000000-0000-4000-8000-0000000000ff";
const MACHINE = "0".repeat(32);

let harness: PostgresTestHarness;
const ids = { admin: "", member: "" };
const sent: string[] = [];
const inngest = asTestDouble<Inngest>()({
  send: async (event: { name: string }) => {
    sent.push(event.name);
    return { ids: [] };
  },
});

/** A Cluster whose first report never arrives. */
const silentCluster = asTestDouble<OrganizationRuntimeService>()({
  cancel: () => Effect.void,
  open: () => Effect.succeed({ status: "connected" as const, connected: asTestDouble<ConnectedRuntimeClient>()({ watchFirstFrame: () => Effect.succeed(null) }) }),
});

type Gated = (actor: Actor) => Effect.Effect<unknown, unknown, never>;
const at = { organizationSlug: SLUG, machineId: MACHINE } as never;
const caller = (userId: string): Caller => ({
  userId, role: userId === ids.admin ? "admin" : "member", organization: { id: LAB, slug: SLUG },
  credential: { kind: "session", id: "s" }, support: null, via: "dashboard",
});

/** Each call, what an admin gets from it in a Lab with no Servers, Cluster or volumes, and the events it sends. */
const gates: ReadonlyArray<readonly [string, Gated, string | RegExp, ReadonlyArray<string>]> = [
  ["requestServerUpgrade", (actor) => requestServerUpgrade(actor, at) as never, "undefined", ["server/upgrade.requested"]],
  ["requestServerWebTraffic", (actor) =>
    requestServerWebTraffic(actor, { organizationSlug: SLUG, machineId: MACHINE, requestId: MISSING, want: "off" } as never) as never, /^\{"change":\{"attemptId":"00000000-0000-4000-8000-0000000000ff","want":"off","state":"pending","requestedAt":"[^"]+"\}\}$/, ["server/web-traffic.requested"]],
  ["requestServerPolicyChange", (actor) =>
    requestServerPolicyChange(actor, { organizationSlug: SLUG, machineId: MACHINE, change: { acceptsServices: true } } as never) as never, "undefined", ["machine/policy-change.requested"]],
  ["planKeyedServerPolicyChange", (actor) => planKeyedServerPolicyChange(caller(actor.userId), MACHINE as never, { acceptsServices: true }) as never, "a plan", []],
  ["requestServerDrain", (actor) =>
    requestServerDrain(caller(actor.userId), { organizationSlug: SLUG, machineId: MACHINE, requestId: MISSING } as never) as never, "Error: Service not found: ployz/OrganizationRuntime", []],
  ["loadNamespaceDataLoss", (actor) => loadNamespaceDataLoss(actor, { organizationSlug: SLUG, namespace: "stray" }) as never, "Error: Service not found: ployz/CloudStore", []],
  ["removeNamespace", (actor) => removeNamespace(actor, { organizationSlug: SLUG, namespace: "stray", confirmDataLoss: [] }) as never, "Error: Service not found: ployz/CloudStore", []],
  ["loadMachineDataLoss", (actor) => loadMachineDataLoss(actor, at) as never, "Error: Service not found: ployz/OrganizationRuntime", []],
  ["enqueueMachineRemove", (actor) => enqueueMachineRemove(actor, at) as never, "Error: Service not found: ployz/OrganizationRuntime", []],
  ["forgetServerFromDashboard", (actor) => forgetServerFromDashboard(caller(actor.userId), MACHINE as never, "forget") as never, "Error: Service not found: ployz/OrganizationRuntime", []],
  ["mintMachineEnrollment", (actor) => mintMachineEnrollment(actor, { organizationSlug: SLUG, ingressClosed: false }) as never, /^\{"token":"pmet_[\w-]+","expiresAt":"[^"]+","ingressClosed":false\}$/, []],
  ["deleteCopy", (actor) =>
    deleteCopy(actor, { organizationSlug: SLUG, volumeId: "v", environment: { project: "shop", environment: "production" }, server: MACHINE, copy: null }).pipe(
      Effect.provideService(OrganizationRuntime, silentCluster)) as never, JSON.stringify({ ok: false, refusal: { code: "unanswered", message: "Your servers aren't answering. Try again once they are.", details: null } }), []],
  ["server_upgrade_policy_set", (actor) => runCommand(caller(actor.userId), "server_upgrade_policy_set", { automatic: true }, { approved: null, key: null }) as never, JSON.stringify({ ok: true, output: { channel: "stable", automatic: true } }), ["server/upgrade.requested"]],
];

const outcomeOf = async (gate: Gated, userId: string) => {
  const exit = await harness.runEffect(Effect.exit(gate({ userId }).pipe(
    Effect.provideService(SupportStaff, supportStaffOf([])),
    Effect.provideService(InngestClient, inngest),
    Effect.provide(testConfigLayer({ DATABASE_URL: "postgres://unused" })),
  )));
  if (Exit.isSuccess(exit)) return exit.value instanceof Function ? "a plan" : JSON.stringify(exit.value) ?? "undefined";
  const error = Cause.squash(exit.cause);
  return error instanceof Error && "_tag" in error ? `${String(error._tag)}: ${error.message}` : String(error);
};

beforeAll(async () => {
  harness = await startPostgresTestHarness();
  const users = await harness.db.insert(user).values([
    { email: "grace@example.test", name: "Grace" },
    { email: "linus@example.test", name: "Linus" },
  ]).returning({ id: user.id });
  [ids.admin, ids.member] = users.map((row) => row.id) as [string, string];
  await harness.db.insert(organization).values({ id: LAB, name: "Lab", slug: SLUG });
  await harness.db.insert(member).values([
    { userId: ids.admin, organizationId: LAB, role: "admin" },
    { userId: ids.member, organizationId: LAB, role: "member" },
  ]);
}, 60_000);

afterAll(async () => {
  await harness?.stop();
});

const REFUSED = "Only an owner or admin of this Organization can do this.";

describe("Server management and copy deletion", () => {
  it.each(gates)("%s refuses a member with the owner-or-admin message", async (name, gate) => {
    sent.length = 0;
    expect([await outcomeOf(gate, ids.member), sent]).toEqual([
      name === "server_upgrade_policy_set"
        ? JSON.stringify({ ok: false, refusal: { code: "forbidden", message: REFUSED, details: null, writes: "none" } })
        : `Forbidden: ${REFUSED}`,
      [],
    ]);
  });

  it.each(gates)("%s answers an admin", async (_name, gate, outcome, events) => {
    sent.length = 0;
    expect([await outcomeOf(gate, ids.admin), sent]).toEqual([
      outcome instanceof RegExp ? expect.stringMatching(outcome) : outcome,
      events,
    ]);
  });

  it("lets support set the upgrade policy, as an admin can", async () => {
    sent.length = 0;
    const support: Caller = { ...caller(ids.member), role: null, support: { variables: false, configs: false, logs: false } };
    const outcome = await outcomeOf(() => runCommand(support, "server_upgrade_policy_set", { automatic: false }, { approved: null, key: null }) as never, ids.member);
    expect([outcome, sent]).toEqual([JSON.stringify({ ok: true, output: { channel: "stable", automatic: false } }), []]);
  });

  it("refuses support Server access and member management, as trunk did", async () => {
    const support: Caller = { ...caller(ids.admin), role: null, support: { variables: true, configs: true, logs: true } };
    const access = await outcomeOf(() => provideServerAccess(support as never) as never, ids.admin);
    const invite = await outcomeOf(() => runCommand(support, "org_member_invite", {}, { approved: null, key: null }) as never, ids.admin);
    expect([JSON.parse(access).refusal.code, JSON.parse(invite).refusal.code]).toEqual(["forbidden", "forbidden"]);
  });
});
