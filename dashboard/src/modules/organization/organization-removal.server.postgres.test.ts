import { eq } from "drizzle-orm";
import { Effect } from "effect";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { CloudStore } from "#/modules/config-store/store-sdk.server";
import { member, user } from "#/modules/identity/tables";
import { removeOrganizationAsMember } from "#/modules/organization/organization-removal.server";
import { organization } from "#/modules/organization/tables";
import { OrganizationRuntime, type OrganizationRuntimeService } from "#/modules/runtime/organization-runtime.server";
import { Ployz, type PloyzService } from "#/modules/runtime/ployz.server";
import { startPostgresTestHarness, type PostgresTestHarness } from "#/test/postgres";

const LAB = "00000000-0000-4000-8000-00000000c001";

let harness: PostgresTestHarness;
let admin: string;
let plain: string;

beforeAll(async () => {
  harness = await startPostgresTestHarness();
  const users = await harness.db.insert(user).values([
    { email: "ada@example.test", name: "Ada" },
    { email: "bob@example.test", name: "Bob" },
    { email: "carol@example.test", name: "Carol" },
  ]).returning({ id: user.id });
  const [owner, adminId, plainId] = users.map((row) => row.id) as [string, string, string];
  admin = adminId;
  plain = plainId;
  await harness.db.insert(organization).values({ id: LAB, name: "Lab", slug: "lab" });
  await harness.db.insert(member).values([
    { userId: owner, organizationId: LAB, role: "owner" },
    { userId: admin, organizationId: LAB, role: "admin" },
    { userId: plain, organizationId: LAB, role: "member" },
  ]);
}, 60_000);

afterAll(async () => {
  await harness?.stop();
});

describe("removeOrganizationAsMember", () => {
  it.each([["an admin", () => admin], ["a member", () => plain]])("refuses %s with the owner message", async (_, userId) => {
    // The refusal comes before the Store, the runtime or a Server is touched.
    const result = await harness.runEffect(removeOrganizationAsMember({ userId: userId() }, "lab").pipe(
      Effect.provideService(CloudStore, { open: Effect.die("the Store was opened") }),
      Effect.provideService(OrganizationRuntime, asTestDouble<OrganizationRuntimeService>()({})),
      Effect.provideService(Ployz, asTestDouble<PloyzService>()({})),
    ));
    expect(result).toEqual({
      ok: false,
      refusal: { code: "forbidden", message: "Only an owner of this Organization can do this.", details: null },
    });
    expect(await harness.db.select().from(organization).where(eq(organization.id, LAB))).toHaveLength(1);
  });
});
