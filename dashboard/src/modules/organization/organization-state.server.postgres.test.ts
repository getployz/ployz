import { eq } from "drizzle-orm";
import { Schema } from "effect";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { member, session, user } from "#/modules/identity/tables";
import { startPostgresTestHarness, type PostgresTestHarness } from "#/test/postgres";
import { CreateOrganization } from "./organization-state";
import { createOrganization } from "./organization-state.server";
import { organization } from "./tables";

let harness: PostgresTestHarness;
let actor: string;

const create = (input: { name: string; slug: string }) =>
  harness.runEffect(createOrganization({ userId: actor }, Schema.decodeUnknownSync(CreateOrganization)(input)));

beforeAll(async () => {
  harness = await startPostgresTestHarness();
  const rows = await harness.db.insert(user).values({ email: "ada@example.test", name: "Ada" }).returning({ id: user.id });
  [actor] = rows.map((row) => row.id) as [string];
  await harness.db.insert(session).values({ userId: actor, token: "t", expiresAt: new Date(Date.now() + 60_000) });
}, 60_000);

afterAll(async () => {
  await harness?.stop();
});

describe("createOrganization", () => {
  it("makes the actor its owner, suffixes a taken slug, and opens it for the actor's sessions", async () => {
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
    const [opened] = await harness.db.select().from(session).where(eq(session.userId, actor));
    expect(opened).toMatchObject({ activeOrganizationId: created.id, activeOrganizationSlug: second });
  });
});
