import { eq } from "drizzle-orm";
import { Cause, Effect, Exit } from "effect";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { readCollection } from "#/collections/read.server";
import { currentChangeCursor } from "#/modules/organization/change-log.server";
import { handleOrgChangesRequest } from "#/routes/api/org/-changes.handler";
import { readOrganizationChanges } from "#/routes/api/org/-changes.server";
import { organizationClusterDomain } from "#/modules/cluster-domain/tables";
import { startFakeHostedDns } from "#/modules/cluster-domain/hosted-dns.test-fixture";
import { CloudStore, openCloudStore, storeTry } from "#/modules/config-store/store-sdk.server";
import { InngestClient } from "#/modules/inngest/client";
import { member, session, user } from "#/modules/identity/tables";
import { requireAccess, SupportStaff, supportStaffOf } from "#/modules/organization/access.server";
import { presetGrant } from "#/modules/identity/token-grant";
import { removeOrganization, removeOrganizationAsMember } from "#/modules/organization/organization-removal.server";
import { organization } from "#/modules/organization/tables";
import { OrganizationRuntime, type OrganizationRuntimeService } from "#/modules/runtime/organization-runtime.server";
import { Ployz, type PloyzService } from "#/modules/runtime/ployz.server";
import { organizationSupportAccess } from "#/modules/support-access/tables";
import { startPostgresTestHarness, type PostgresTestHarness } from "#/test/postgres";
import { makeSecretEncryption } from "#/utils/encrypted-secret.server";
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
  const NAMED = "00000000-0000-4000-8000-00000000c006";

  const removeOwned = (ownerId: string, organizationId: string, slug: string) => harness.runEffect(Effect.gen(function* () {
    const store = yield* openCloudStore(harness.databaseUrl, harness.database, "test-encryption-secret");
    return yield* removeOrganization({ userId: ownerId, organization: { id: organizationId, slug }, role: "owner" }, organizationId).pipe(
      Effect.provideService(CloudStore, { open: Effect.succeed(store) }),
      Effect.provideService(OrganizationRuntime, asTestDouble<OrganizationRuntimeService>()({})),
      Effect.provideService(Ployz, asTestDouble<PloyzService>()({})),
      Effect.provideService(InngestClient, sendsNowhere()),
    );
  }));

  it("releases its Cluster Domain at the Hosted DNS endpoint that granted it", async () => {
    const hostedDns = await startFakeHostedDns();
    try {
      const [fay] = (await harness.db.insert(user).values({ email: "fay@example.test", name: "Fay" }).returning({ id: user.id }))
        .map((row) => row.id) as [string];
      await harness.db.insert(organization).values({ id: NAMED, name: "Named", slug: "named" });
      await harness.db.insert(member).values({ userId: fay, organizationId: NAMED, role: "owner" });
      const now = new Date();
      await harness.db.insert(organizationClusterDomain).values({
        organizationId: NAMED, endpoint: hostedDns.url, name: "named.ployz.test",
        encryptedToken: makeSecretEncryption("test-encryption-secret").encrypt("named-token"), reservedAt: now, leaseRenewedAt: now,
      });
      expect(await removeOwned(fay, NAMED, "named")).toMatchObject({ ok: true, value: { removed: true } });
      expect(hostedDns.requests).toEqual([
        { method: "DELETE", path: "/domains/named.ployz.test", authorization: "Bearer named-token", body: null },
      ]);
      expect(await harness.db.select().from(organizationClusterDomain).where(eq(organizationClusterDomain.organizationId, NAMED))).toEqual([]);
    } finally {
      await hostedDns.close();
    }
  });

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
    const result = await removeOwned(dan, SHOP, "shop");
    expect(result).toMatchObject({ ok: true, value: { removed: true } });
    const sessions = await harness.db.select({ token: session.token, id: session.activeOrganizationId, slug: session.activeOrganizationSlug })
      .from(session).orderBy(session.token);
    expect(sessions).toEqual([
      { token: "dan-later", id: LATER, slug: "later" },
      { token: "dan-shop", id: HOME, slug: "home" },
      { token: "eve-shop", id: null, slug: null },
    ]);
  });

  it("frees the slug, and an old link to it opens the new Organization only for its members", async () => {
    const KILN = "00000000-0000-4000-8000-00000000c00a";
    const NEW_KILN = "00000000-0000-4000-8000-00000000c00b";
    const [ivy, gus] = (await harness.db.insert(user).values([
      { email: "ivy@example.test", name: "Ivy" },
      { email: "gus@example.test", name: "Gus" },
    ]).returning({ id: user.id })).map((row) => row.id) as [string, string];
    await harness.db.insert(organization).values({ id: KILN, name: "Kiln", slug: "kiln" });
    await harness.db.insert(member).values({ userId: ivy, organizationId: KILN, role: "owner" });
    const store = await harness.runEffect(openCloudStore(harness.databaseUrl, harness.database, "test-encryption-secret"));
    const result = await harness.runEffect(removeOrganization({ userId: ivy, organization: { id: KILN, slug: "kiln" }, role: "owner" }, KILN).pipe(
      Effect.provideService(CloudStore, { open: Effect.succeed(store) }),
      Effect.provideService(OrganizationRuntime, asTestDouble<OrganizationRuntimeService>()({})),
      Effect.provideService(Ployz, asTestDouble<PloyzService>()({})),
      Effect.provideService(InngestClient, sendsNowhere()),
    ));
    expect(result).toMatchObject({ ok: true, value: { removed: true } });

    await harness.db.insert(organization).values({ id: NEW_KILN, name: "Kiln", slug: "kiln" });
    await harness.db.insert(member).values({ userId: gus, organizationId: NEW_KILN, role: "owner" });
    const open = (userId: string) => harness.runEffect(Effect.exit(requireAccess({ userId }, "kiln", "org:read").pipe(
      Effect.map((access) => access.organization.id),
      Effect.provideService(SupportStaff, supportStaffOf([])),
    )));
    const former = await open(ivy);
    expect(Exit.isFailure(former) && Cause.pretty(former.cause)).toContain("The organization was not found.");
    expect(await open(gus)).toEqual(Exit.succeed(NEW_KILN));
  });

  it("reads a new Organization on a reused slug in full from the deleted one's cursor", async () => {
    const OVEN = "00000000-0000-4000-8000-00000000c00c";
    const NEW_OVEN = "00000000-0000-4000-8000-00000000c00d";
    const [hal] = (await harness.db.insert(user).values({ email: "hal@example.test", name: "Hal" }).returning({ id: user.id })).map((row) => row.id) as [string];
    await harness.db.insert(organization).values({ id: OVEN, name: "Oven", slug: "oven" });
    await harness.db.insert(member).values({ userId: hal, organizationId: OVEN, role: "owner" });
    await harness.db.insert(organizationSupportAccess).values({ organizationId: OVEN, allowed: true });
    const readSupport = (since?: string) => harness.runEffect(readCollection({ userId: hal }, {
      table: "organization_support_access", userId: hal, organizationSlug: "oven", since,
    }).pipe(Effect.provideService(SupportStaff, supportStaffOf([]))));
    const cached = await readSupport();
    expect(cached).toMatchObject({ full: true, rows: [{ id: OVEN, allowed: true }] });

    const store = await harness.runEffect(openCloudStore(harness.databaseUrl, harness.database, "test-encryption-secret"));
    const result = await harness.runEffect(removeOrganization({ userId: hal, organization: { id: OVEN, slug: "oven" }, role: "owner" }, OVEN).pipe(
      Effect.provideService(CloudStore, { open: Effect.succeed(store) }),
      Effect.provideService(OrganizationRuntime, asTestDouble<OrganizationRuntimeService>()({})),
      Effect.provideService(Ployz, asTestDouble<PloyzService>()({})),
      Effect.provideService(InngestClient, sendsNowhere()),
    ));
    expect(result).toMatchObject({ ok: true, value: { removed: true } });
    await harness.db.insert(organization).values({ id: NEW_OVEN, name: "Oven", slug: "oven" });
    await harness.db.insert(member).values({ userId: hal, organizationId: NEW_OVEN, role: "owner" });

    expect(await readSupport(cached.cursor)).toMatchObject({ full: true, rows: [] });
  });

  it("ends a change stream still open on a removed Organization, so the client reconnects to the slug's new one", async () => {
    const KETTLE = "00000000-0000-4000-8000-00000000c00e";
    const NEW_KETTLE = "00000000-0000-4000-8000-00000000c00f";
    const [jo] = (await harness.db.insert(user).values({ email: "jo@example.test", name: "Jo" }).returning({ id: user.id })).map((row) => row.id) as [string];
    await harness.db.insert(organization).values({ id: KETTLE, name: "Kettle", slug: "kettle" });
    await harness.db.insert(member).values({ userId: jo, organizationId: KETTLE, role: "owner" });
    const response = await handleOrgChangesRequest(new Request("http://localhost/api/org/changes?organizationSlug=kettle"), "kettle", {
      authorize: (slug) => harness.runEffect(requireAccess({ userId: jo }, slug, "org:read").pipe(
        Effect.map((access) => ({ organizationId: access.organization.id })),
        Effect.provideService(SupportStaff, supportStaffOf([])),
      )),
      currentCursor: () => harness.runEffect(currentChangeCursor()),
      readChanges: (input) => harness.runEffect(readOrganizationChanges(input)),
    });
    expect(response.status).toBe(200);
    const reader = response.body?.getReader();
    const ended = (async () => {
      while (!(await reader?.read())?.done);
      return "ended";
    })();

    const store = await harness.runEffect(openCloudStore(harness.databaseUrl, harness.database, "test-encryption-secret"));
    const result = await harness.runEffect(removeOrganization({ userId: jo, organization: { id: KETTLE, slug: "kettle" }, role: "owner" }, KETTLE).pipe(
      Effect.provideService(CloudStore, { open: Effect.succeed(store) }),
      Effect.provideService(OrganizationRuntime, asTestDouble<OrganizationRuntimeService>()({})),
      Effect.provideService(Ployz, asTestDouble<PloyzService>()({})),
      Effect.provideService(InngestClient, sendsNowhere()),
    ));
    expect(result).toMatchObject({ ok: true, value: { removed: true } });
    await harness.db.insert(organization).values({ id: NEW_KETTLE, name: "Kettle", slug: "kettle" });
    await harness.db.insert(member).values({ userId: jo, organizationId: NEW_KETTLE, role: "owner" });

    expect(await ended).toBe("ended");
  });

  it("refuses while it read a Project, even one removed before the Store is asked, and asks no one", async () => {
    const FORGE = "00000000-0000-4000-8000-00000000c010";
    const [gil] = (await harness.db.insert(user).values({ email: "gil@example.test", name: "Gil" }).returning({ id: user.id }))
      .map((row) => row.id) as [string];
    await harness.db.insert(organization).values({ id: FORGE, name: "Forge", slug: "forge" });
    await harness.db.insert(member).values({ userId: gil, organizationId: FORGE, role: "owner" });
    const asked: string[] = [];
    const result = await harness.runEffect(Effect.gen(function* () {
      const store = yield* openCloudStore(harness.databaseUrl, harness.database, "test-encryption-secret");
      yield* Effect.promise(() => store.write(FORGE, {
        command: "create_project", id: "00000000-0000-4000-8000-00000000c011", name: "shop", default_environment: "00000000-0000-4000-8000-00000000c012",
      }));
      // The Store's own refusal, which the removal answers with in its place.
      const storeRefusal = yield* storeTry(() => store.removeOrganization(FORGE)).pipe(Effect.flip, Effect.map((error) => error.refusal));
      // A separately approved `project rm` lands right after the removal reads the Projects.
      const read = store.read.bind(store);
      store.read = async (...args) => {
        const projects = await read(...args);
        await store.write(FORGE, { command: "remove", project: "shop", environment: null, version: null, accept_volume_loss: {}, every_copy: {} });
        return projects;
      };
      const removed = yield* removeOrganization({ userId: gil, organization: { id: FORGE, slug: "forge" }, role: "owner" }, FORGE, {
        gate: (operation) => Effect.sync(() => {
          asked.push(operation.verb);
          return { ok: true as const, approvalId: null };
        }),
      }).pipe(
        Effect.provideService(CloudStore, { open: Effect.succeed(store) }),
        Effect.provideService(OrganizationRuntime, asTestDouble<OrganizationRuntimeService>()({})),
        Effect.provideService(Ployz, asTestDouble<PloyzService>()({})),
        Effect.provideService(InngestClient, sendsNowhere()),
      );
      return { removed, storeRefusal };
    }));
    expect(result.removed).toEqual({ ok: false, refusal: result.storeRefusal });
    expect(result.storeRefusal).toMatchObject({ code: "conflict", details: { next: { tool: "remove", input: { project: "shop" } } } });
    expect(asked).toEqual([]);
    expect(await harness.db.select().from(organization).where(eq(organization.id, FORGE))).toHaveLength(1);
  });
});
