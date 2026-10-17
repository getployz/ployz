import { Cause, Effect, Exit } from "effect";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import type { Actor, Caller } from "#/modules/identity/actor";
import { member, user } from "#/modules/identity/tables";
import { enqueueMachineRemove, loadMachineDataLoss } from "#/modules/machines/machine-removal.server";
import { loadNamespaceDataLoss, removeNamespace } from "#/modules/machines/namespace-cleanup.server";
import { planKeyedServerPolicyChange, requestServerPolicyChange } from "#/modules/machines/server-policy.server";
import { requestServerWebTraffic } from "#/modules/machines/server-web-traffic.server";
import { cancelServerEnrollment, startServerEnrollment } from "#/modules/machines/waiting/enroll.server";
import { createFleetToken, revokeFleetToken } from "#/modules/machines/waiting/fleet-token.server";
import { openProbeSelection } from "#/modules/machines/waiting/probe.server";
import { rejectWaitingServers } from "#/modules/machines/waiting/reject.server";
import { requireAccess, SupportStaff, supportStaffOf } from "#/modules/organization/access.server";
import { organization } from "#/modules/organization/tables";
import { requestServerUpgrade } from "#/modules/server-upgrade/server-upgrade.server";
import { startPostgresTestHarness, type PostgresTestHarness } from "#/test/postgres";

const LAB = "00000000-0000-4000-8000-00000000c002";
const SLUG = "lab";

let harness: PostgresTestHarness;
const ids = { admin: "", member: "" };

/** The Lab has no Servers, so past the gate each call fails on something else; only a Forbidden counts as refused. */
type Gated = (actor: Actor) => Effect.Effect<unknown, unknown, never>;
const at = { organizationSlug: SLUG } as never;
const caller = (userId: string, role: Caller["role"]) => ({
  userId, role, organization: { id: LAB, slug: SLUG }, credential: { kind: "session", id: "s" }, support: null, via: "dashboard",
}) satisfies Caller;

const gates: ReadonlyArray<readonly [string, Gated]> = [
  ["requestServerUpgrade", (actor) => requestServerUpgrade(actor, at) as never],
  ["requestServerWebTraffic", (actor) => requestServerWebTraffic(actor, at) as never],
  ["requestServerPolicyChange", (actor) =>
    requestServerPolicyChange(actor, { organizationSlug: SLUG, machineId: "m", change: { acceptsServices: true } } as never) as never],
  ["planKeyedServerPolicyChange", (actor) =>
    planKeyedServerPolicyChange(caller(actor.userId, actor.userId === ids.admin ? "admin" : "member"), "m" as never,
      { acceptsServices: true }) as never],
  ["loadNamespaceDataLoss", (actor) => loadNamespaceDataLoss(actor, { organizationSlug: SLUG, namespace: "stray" }) as never],
  ["removeNamespace", (actor) => removeNamespace(actor, { organizationSlug: SLUG, namespace: "stray", confirmDataLoss: [] }) as never],
  ["loadMachineDataLoss", (actor) => loadMachineDataLoss(actor, at) as never],
  ["enqueueMachineRemove", (actor) => enqueueMachineRemove(actor, at) as never],
  ["createFleetToken", (actor) => createFleetToken(actor, at) as never],
  ["revokeFleetToken", (actor) => revokeFleetToken(actor, { organizationSlug: SLUG, id: "t" }) as never],
  ["openProbeSelection", (actor) => openProbeSelection(actor, { organizationSlug: SLUG, ids: [] }) as never],
  ["rejectWaitingServers", (actor) => rejectWaitingServers(actor, { organizationSlug: SLUG, ids: [] }) as never],
  ["startServerEnrollment", (actor) => startServerEnrollment(actor, at) as never],
  ["cancelServerEnrollment", (actor) => cancelServerEnrollment(actor, { organizationSlug: SLUG, id: "e" }) as never],
  ["volumes:delete-copy", (actor) => requireAccess(actor, SLUG, "volumes:delete-copy") as never],
];

const tagOf = async (gate: Gated, userId: string) => {
  const exit = await harness.runEffect(Effect.exit(gate({ userId }).pipe(Effect.provideService(SupportStaff, supportStaffOf([])))));
  if (Exit.isSuccess(exit)) return "passed";
  const error = Cause.squash(exit.cause);
  return error instanceof Error && "_tag" in error && error._tag === "Forbidden" ? `Forbidden: ${error.message}` : "passed";
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

describe("Server management and copy deletion", () => {
  it.each(gates)("%s refuses a member with the owner-or-admin message", async (_, gate) => {
    expect(await tagOf(gate, ids.member)).toBe("Forbidden: Only an owner or admin of this Organization can do this.");
  });

  it.each(gates)("%s lets an admin past the gate", async (_, gate) => {
    expect(await tagOf(gate, ids.admin)).toBe("passed");
  });
});
