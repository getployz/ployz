import { eq } from "drizzle-orm";
import { Effect } from "effect";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { member, user } from "#/modules/identity/tables";
import { SupportStaff, supportStaffOf } from "#/modules/organization/access.server";
import { organization } from "#/modules/organization/tables";
import type { SupportAccessChange } from "#/modules/support-access/support-access";
import { setSupportAccess } from "#/modules/support-access/support-access.server";
import { organizationSupportAccess } from "#/modules/support-access/tables";
import { startPostgresTestHarness, type PostgresTestHarness } from "#/test/postgres";

const ORGANIZATION = "00000000-0000-4000-8000-00000000a002";
const SLUG = "globex";

let harness: PostgresTestHarness;
let owner: string;
let staff: string;

const set = (userId: string, change: SupportAccessChange) => harness.runEffect(
  setSupportAccess({ userId }, { organizationSlug: SLUG, change }).pipe(
    Effect.map((row) => ({ ok: true as const, row })),
    Effect.catchTags({ Forbidden: (error) => Effect.succeed({ ok: false as const, error: error._tag }) }),
    Effect.provideService(SupportStaff, supportStaffOf([staff])),
  ),
);

const stored = async () => {
  const [row] = await harness.db.select({
    allowed: organizationSupportAccess.allowed,
    variables: organizationSupportAccess.variables,
    configs: organizationSupportAccess.configs,
    logs: organizationSupportAccess.logs,
  }).from(organizationSupportAccess).where(eq(organizationSupportAccess.organizationId, ORGANIZATION));
  return row;
};

beforeAll(async () => {
  harness = await startPostgresTestHarness();
  const users = await harness.db.insert(user).values([
    { email: "owner@globex.test", name: "Owner" },
    { email: "staff@globex.test", name: "Staff" },
  ]).returning({ id: user.id });
  [owner, staff] = users.map((row) => row.id) as [string, string];
  await harness.db.insert(organization).values({ id: ORGANIZATION, name: "Globex", slug: SLUG });
  await harness.db.insert(member).values({ userId: owner, organizationId: ORGANIZATION, role: "owner" });
}, 60_000);

afterAll(async () => {
  await harness?.stop();
});

describe("setting what support may read", () => {
  it("turns every switch off when support access goes off, and back on leaves them off", async () => {
    await set(owner, { setting: "allowed", on: true });
    await set(owner, { setting: "variables", on: true });
    await set(owner, { setting: "configs", on: true });
    await set(owner, { setting: "logs", on: true });
    expect(await stored()).toEqual({ allowed: true, variables: true, configs: true, logs: true });

    const off = await set(owner, { setting: "allowed", on: false });
    expect(off).toEqual({ ok: true, row: { id: ORGANIZATION, allowed: false, variables: false, configs: false, logs: false } });
    expect(await stored()).toEqual({ allowed: false, variables: false, configs: false, logs: false });

    await set(owner, { setting: "allowed", on: true });
    expect(await stored()).toEqual({ allowed: true, variables: false, configs: false, logs: false });
  });

  it("ignores turning a switch on while support access is off", async () => {
    await set(owner, { setting: "allowed", on: false });
    const result = await set(owner, { setting: "variables", on: true });
    expect(result).toEqual({ ok: true, row: { id: ORGANIZATION, allowed: false, variables: false, configs: false, logs: false } });
    expect(await stored()).toEqual({ allowed: false, variables: false, configs: false, logs: false });
  });

  it("refuses support changing what it may read, even while it is let in", async () => {
    await set(owner, { setting: "allowed", on: true });
    expect(await set(staff, { setting: "variables", on: true })).toEqual({ ok: false, error: "Forbidden" });
    expect(await set(staff, { setting: "allowed", on: false })).toEqual({ ok: false, error: "Forbidden" });
    expect(await stored()).toEqual({ allowed: true, variables: false, configs: false, logs: false });
  });
});
