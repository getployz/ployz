import { eq } from "drizzle-orm";
import { Effect, Schema } from "effect";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { member, session, user } from "#/modules/identity/tables";
import { SupportStaff, supportStaffOf } from "#/modules/organization/access.server";
import { organizationSupportAccess } from "#/modules/support-access/tables";
import { startPostgresTestHarness, type PostgresTestHarness } from "#/test/postgres";
import { CreateOrganization, RenameOrganization } from "./organization-state";
import { createOrganization, renameOrganization } from "./organization-state.server";
import { organization } from "./tables";

let harness: PostgresTestHarness;
let actor: string;
let browser: string;
let device: string;

const create = (input: { name: string; slug: string }) =>
  harness.runEffect(createOrganization(
    { userId: actor, credential: { kind: "session", id: browser } },
    Schema.decodeUnknownSync(CreateOrganization)(input),
  ));

const sessionRow = async (id: string) => {
  const [row] = await harness.db.select().from(session).where(eq(session.id, id));
  return row;
};

beforeAll(async () => {
  harness = await startPostgresTestHarness();
  const rows = await harness.db.insert(user).values({ email: "ada@example.test", name: "Ada" }).returning({ id: user.id });
  [actor] = rows.map((row) => row.id) as [string];
  const sessions = await harness.db.insert(session).values([
    { userId: actor, token: "browser", expiresAt: new Date(Date.now() + 60_000) },
    { userId: actor, token: "device", expiresAt: new Date(Date.now() + 60_000), updatedAt: new Date(Date.UTC(2026, 0, 1)) },
  ]).returning({ id: session.id });
  [browser, device] = sessions.map((row) => row.id) as [string, string];
}, 60_000);

afterAll(async () => {
  await harness?.stop();
});

describe("createOrganization", () => {
  it("makes the actor its owner, suffixes a taken slug, and opens it for the actor's session only", async () => {
    expect(await create({ name: "  Acme  ", slug: "Acme Labs" })).toBe("acme-labs");
    const second = await create({ name: "Acme", slug: "acme-labs" });
    expect(second).toMatch(/^acme-labs-[0-9a-f]{8}$/u);

    const [first] = await harness.db.select().from(organization).where(eq(organization.slug, "acme-labs"));
    expect(first?.name).toBe("Acme");
    const [created] = await harness.db.select().from(organization).where(eq(organization.slug, second));
    if (!created) throw new Error(`no organization ${second}`);
    const members = await harness.db.select({ userId: member.userId, role: member.role }).from(member)
      .where(eq(member.organizationId, created.id));
    expect(members).toEqual([{ userId: actor, role: "owner" }]);
    expect(await sessionRow(browser)).toMatchObject({ activeOrganizationId: created.id, activeOrganizationSlug: second });
    expect(await sessionRow(device)).toMatchObject({
      activeOrganizationId: null, activeOrganizationSlug: null, updatedAt: new Date(Date.UTC(2026, 0, 1)),
    });
  });

  it("never takes a slug a Cloud page uses", async () => {
    expect(await create({ name: "Account", slug: "Account" })).toMatch(/^account-[0-9a-f]{8}$/u);
  });
});

describe("renameOrganization", () => {
  let staff = "";
  const rename = (userId: string, input: { organizationSlug: string; name: string }) => harness.runEffect(
    renameOrganization({ userId }, Schema.decodeUnknownSync(RenameOrganization)(input)).pipe(
      Effect.map((renamed) => ({ ok: true as const, ...renamed })),
      Effect.catchTags({ Forbidden: (error) => Effect.succeed({ ok: false as const, error: error._tag }) }),
      Effect.provideService(SupportStaff, supportStaffOf([staff])),
    ),
  );
  const named = async (slug: string) =>
    (await harness.db.select({ name: organization.name }).from(organization).where(eq(organization.slug, slug)))[0]?.name;

  it("renames for an owner or admin and keeps the slug; a member is refused", async () => {
    const slug = await create({ name: "Initech", slug: "initech" });
    const [created] = await harness.db.select({ id: organization.id }).from(organization).where(eq(organization.slug, slug));
    if (!created) throw new Error(`no organization ${slug}`);
    const others = await harness.db.insert(user).values([
      { email: "bill@example.test", name: "Bill" },
      { email: "milton@example.test", name: "Milton" },
    ]).returning({ id: user.id });
    const [admin, plain] = others.map((row) => row.id) as [string, string];
    await harness.db.insert(member).values([
      { userId: admin, organizationId: created.id, role: "admin" },
      { userId: plain, organizationId: created.id, role: "member" },
    ]);

    expect(await rename(actor, { organizationSlug: slug, name: "  Initrode  " })).toEqual({ ok: true, name: "Initrode" });
    expect(await rename(admin, { organizationSlug: slug, name: "Initech Labs" })).toEqual({ ok: true, name: "Initech Labs" });
    expect(await rename(plain, { organizationSlug: slug, name: "Milton's" })).toEqual({ ok: false, error: "Forbidden" });
    expect(await named(slug)).toBe("Initech Labs");
  });

  it("refuses a support visit, even where the Organization lets support in", async () => {
    const slug = await create({ name: "Globex", slug: "globex" });
    const [created] = await harness.db.select({ id: organization.id }).from(organization).where(eq(organization.slug, slug));
    if (!created) throw new Error(`no organization ${slug}`);
    const [row] = await harness.db.insert(user).values({ email: "staff@example.test", name: "Staff" }).returning({ id: user.id });
    if (!row) throw new Error("no staff user");
    staff = row.id;
    await harness.db.insert(organizationSupportAccess).values({ organizationId: created.id, allowed: true });

    expect(await rename(staff, { organizationSlug: slug, name: "Hijacked" })).toEqual({ ok: false, error: "Forbidden" });
    expect(await named(slug)).toBe("Globex");
  });

  it("takes the same names an Organization is created with", () => {
    const decode = Schema.decodeUnknownSync(RenameOrganization);
    expect(() => decode({ organizationSlug: "initech", name: "   " })).toThrow();
    expect(() => decode({ organizationSlug: "initech", name: "x".repeat(65) })).toThrow();
  });
});
