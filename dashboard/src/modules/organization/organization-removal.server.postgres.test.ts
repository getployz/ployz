import { eq } from "drizzle-orm";
import { Cause, Effect, Exit } from "effect";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { CloudStore, openCloudStore } from "#/modules/config-store/store-sdk.server";
import { InngestClient } from "#/modules/inngest/client";
import { member, session, user } from "#/modules/identity/tables";
import { SupportStaff, supportStaffOf } from "#/modules/organization/access.server";
import { presetGrant } from "#/modules/identity/token-grant";
import { removeOrganization, removeOrganizationAsMember } from "#/modules/organization/organization-removal.server";
import { organization } from "#/modules/organization/tables";
import { OrganizationRuntime, type OrganizationRuntimeService } from "#/modules/runtime/organization-runtime.server";
import { Ployz, type PloyzService } from "#/modules/runtime/ployz.server";
import { organizationSupportAccess } from "#/modules/support-access/tables";
import { startPostgresTestHarness, type PostgresTestHarness } from "#/test/postgres";
import { seedAnonymousStoreOrganization, sendsNowhere } from "#/test/store-cloud";

const LAB = "00000000-0000-4000-8000-00000000c001";
const SANDBOX = "00000000-0000-4000-8000-00000000c002";
const STORE_OPENED = "the Store was opened";

let harness: PostgresTestHarness;
const ids = { owner: "", admin: "", member: "", support: "", agent: "" };

// An owner gets past the gate and opens the Store, which this double turns into a defect; a refusal opens nothing.
const removeAs = (who: keyof typeof ids, slug = "lab") => harness.runEffect(Effect.exit(removeOrganizationAsMember({ userId: ids[who] }, slug).pipe(
  Effect.provideService(CloudStore, { open: Effect.die(STORE_OPENED) }),
  Effect.provideService(OrganizationRuntime, asTestDouble<OrganizationRuntimeService>()({})),
  Effect.provideService(Ployz, asTestDouble<PloyzService>()({})),
  Effect.provideService(SupportStaff, supportStaffOf([ids.support])),
  Effect.provideService(InngestClient, sendsNowhere()),
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
  ids.agent = (await harness.runEffect(seedAnonymousStoreOrganization(SANDBOX, new Date(Date.now() - 1000)))).userId;
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

  it("refuses an owner's token whose grant leaves out org:delete, naming it", async () => {
    const caller = {
      userId: ids.owner,
      organization: { id: LAB, slug: "lab" },
      role: "owner" as const,
      credential: { kind: "token" as const, id: "00000000-0000-4000-8000-00000000c002", grant: presetGrant({ preset: "read" }) },
    };
    expect(await harness.runEffect(Effect.exit(removeOrganization(caller, LAB).pipe(
      Effect.provideService(CloudStore, { open: Effect.die(STORE_OPENED) }),
      Effect.provideService(OrganizationRuntime, asTestDouble<OrganizationRuntimeService>()({})),
      Effect.provideService(Ployz, asTestDouble<PloyzService>()({})),
      Effect.provideService(InngestClient, sendsNowhere()),
    )))).toEqual(Exit.succeed({
      ok: false,
      refusal: { code: "forbidden", message: "This token's grant doesn't include org:delete. No changes made.", details: { permission: "org:delete" } },
    }));
  });

  it("lets an owner through to the removal", async () => {
    const exit = await removeAs("owner");
    expect(Exit.isFailure(exit) && Cause.pretty(exit.cause)).toContain(STORE_OPENED);
  });

  it("lets an anonymous owner through to the removal of its sandbox", async () => {
    const exit = await removeAs("agent", `anon-${SANDBOX.slice(-8)}`);
    expect(Exit.isFailure(exit) && Cause.pretty(exit.cause)).toContain(STORE_OPENED);
  });

  it("shows an anonymous owner nothing of someone else's Organization", async () => {
    const exit = await removeAs("agent");
    expect(Exit.isFailure(exit) && Cause.pretty(exit.cause)).toContain("The organization was not found.");
    expect(await harness.db.select().from(organization).where(eq(organization.id, LAB))).toHaveLength(1);
  });
});

describe("removeOrganization", () => {
  const SHOP = "00000000-0000-4000-8000-00000000c003";
  const HOME = "00000000-0000-4000-8000-00000000c004";
  const LATER = "00000000-0000-4000-8000-00000000c005";

  it("moves each session acting in it to its user's next Organization, or none", async () => {
    const [dan, eve] = (await harness.db.insert(user).values([
      { email: "dan@example.test", name: "Dan" },
      { email: "eve@example.test", name: "Eve" },
    ]).returning({ id: user.id })).map((row) => row.id) as [string, string];
    await harness.db.insert(organization).values([
      { id: HOME, name: "Home", slug: "home" },
      { id: SHOP, name: "Shop", slug: "shop" },
      { id: LATER, name: "Later", slug: "later" },
    ]);
    await harness.db.insert(member).values([
      { userId: dan, organizationId: HOME, role: "owner", createdAt: new Date("2026-01-01") },
      { userId: dan, organizationId: SHOP, role: "owner", createdAt: new Date("2026-02-01") },
      { userId: dan, organizationId: LATER, role: "owner", createdAt: new Date("2026-03-01") },
      { userId: eve, organizationId: SHOP, role: "member" },
    ]);
    const expiresAt = new Date(Date.now() + 60_000);
    await harness.db.insert(session).values([
      { userId: dan, token: "dan-shop", expiresAt, activeOrganizationId: SHOP, activeOrganizationSlug: "shop" },
      { userId: dan, token: "dan-later", expiresAt, activeOrganizationId: LATER, activeOrganizationSlug: "later" },
      { userId: eve, token: "eve-shop", expiresAt, activeOrganizationId: SHOP, activeOrganizationSlug: "shop" },
    ]);
    const store = await harness.runEffect(openCloudStore(harness.databaseUrl, harness.database, "test-encryption-secret"));
    const result = await harness.runEffect(removeOrganization({ userId: dan, organization: { id: SHOP, slug: "shop" }, role: "owner" }, SHOP).pipe(
      Effect.provideService(CloudStore, { open: Effect.succeed(store) }),
      Effect.provideService(OrganizationRuntime, asTestDouble<OrganizationRuntimeService>()({})),
      Effect.provideService(Ployz, asTestDouble<PloyzService>()({})),
      Effect.provideService(InngestClient, sendsNowhere()),
    ));
    expect(result).toMatchObject({ ok: true, value: { removed: true } });
    const sessions = await harness.db.select({ token: session.token, id: session.activeOrganizationId, slug: session.activeOrganizationSlug })
      .from(session).orderBy(session.token);
    expect(sessions).toEqual([
      { token: "dan-later", id: LATER, slug: "later" },
      { token: "dan-shop", id: HOME, slug: "home" },
      { token: "eve-shop", id: null, slug: null },
    ]);
  });
});
