import { eq } from "drizzle-orm";
import { Cause, Effect, Exit } from "effect";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { CloudStore } from "#/modules/config-store/store-sdk.server";
import { member, user } from "#/modules/identity/tables";
import { SupportStaff, supportStaffOf } from "#/modules/organization/access.server";
import { removeOrganizationAsMember } from "#/modules/organization/organization-removal.server";
import { organization } from "#/modules/organization/tables";
import { OrganizationRuntime, type OrganizationRuntimeService } from "#/modules/runtime/organization-runtime.server";
import { Ployz, type PloyzService } from "#/modules/runtime/ployz.server";
import { organizationSupportAccess } from "#/modules/support-access/tables";
import { startPostgresTestHarness, type PostgresTestHarness } from "#/test/postgres";

const LAB = "00000000-0000-4000-8000-00000000c001";
const STORE_OPENED = "the Store was opened";

let harness: PostgresTestHarness;
const ids = { owner: "", admin: "", member: "", support: "" };

// An owner gets past the gate and opens the Store, which this double turns into a defect; a refusal opens nothing.
const removeAs = (who: keyof typeof ids) => harness.runEffect(Effect.exit(removeOrganizationAsMember({ userId: ids[who] }, "lab").pipe(
  Effect.provideService(CloudStore, { open: Effect.die(STORE_OPENED) }),
  Effect.provideService(OrganizationRuntime, asTestDouble<OrganizationRuntimeService>()({})),
  Effect.provideService(Ployz, asTestDouble<PloyzService>()({})),
  Effect.provideService(SupportStaff, supportStaffOf([ids.support])),
)));

beforeAll(async () => {
  harness = await startPostgresTestHarness();
  const users = await harness.db.insert(user).values([
    { email: "ada@example.test", name: "Ada" },
    { email: "bob@example.test", name: "Bob" },
    { email: "carol@example.test", name: "Carol" },
    { email: "sam@example.test", name: "Sam" },
  ]).returning({ id: user.id });
  [ids.owner, ids.admin, ids.member, ids.support] = users.map((row) => row.id) as [string, string, string, string];
  await harness.db.insert(organization).values({ id: LAB, name: "Lab", slug: "lab" });
  await harness.db.insert(member).values([
    { userId: ids.owner, organizationId: LAB, role: "owner" },
    { userId: ids.admin, organizationId: LAB, role: "admin" },
    { userId: ids.member, organizationId: LAB, role: "member" },
  ]);
  await harness.db.insert(organizationSupportAccess).values({ organizationId: LAB, allowed: true });
}, 60_000);

afterAll(async () => {
  await harness?.stop();
});

describe("removeOrganizationAsMember", () => {
  it.each(["admin", "member", "support"] as const)("refuses %s with the owner message", async (who) => {
    expect(await removeAs(who)).toEqual(Exit.succeed({
      ok: false,
      refusal: { code: "forbidden", message: "Only an owner of this Organization can do this.", details: null },
    }));
    expect(await harness.db.select().from(organization).where(eq(organization.id, LAB))).toHaveLength(1);
  });

  it("lets an owner through to the removal", async () => {
    const exit = await removeAs("owner");
    expect(Exit.isFailure(exit) && Cause.pretty(exit.cause)).toContain(STORE_OPENED);
  });
});
