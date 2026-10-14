import { Effect } from "effect";
import { afterAll, beforeAll, beforeEach, describe, expect, it } from "vitest";
import type { Caller } from "#/modules/identity/actor";
import { createOrganizationToken, listCredentials, revokeCredential } from "#/modules/identity/organization-token.server";
import { member, organizationToken, user } from "#/modules/identity/tables";
import { organization } from "#/modules/organization/tables";
import type { Database } from "#/server/database.server";
import { startPostgresTestHarness, type PostgresTestHarness } from "#/test/postgres";

const ACME = "00000000-0000-4000-8000-00000000d001";

let harness: PostgresTestHarness;
const ids = { ada: "", bob: "", eve: "" };
const roles = { ada: "owner", bob: "admin", eve: "member" } satisfies Record<keyof typeof ids, Caller["role"]>;

const run = <A, E>(effect: Effect.Effect<A, E, Database>) => harness.runEffect(effect);

const callerOf = (who: keyof typeof ids): Caller => ({
  userId: ids[who],
  organization: { id: ACME, slug: "acme" },
  credential: { kind: "session", id: `${who}-session` },
  role: roles[who],
  support: null,
  via: "dashboard",
});

const tokenOf = async (who: keyof typeof ids) => (await run(createOrganizationToken(callerOf(who), { name: `${who}-ci`, expiresInDays: 1 }))).id;

const listedBy = async (who: keyof typeof ids) => (await run(listCredentials(callerOf(who)))).tokens.map((token) => token.name).sort();

const revokedBy = (who: keyof typeof ids, id: string) => run(revokeCredential(callerOf(who), id).pipe(
  Effect.as("revoked"),
  Effect.catchTag("NotFound", () => Effect.succeed("not_found")),
));

beforeAll(async () => {
  harness = await startPostgresTestHarness();
  const users = await harness.db.insert(user).values([
    { email: "ada@example.test", name: "Ada" },
    { email: "bob@example.test", name: "Bob" },
    { email: "eve@example.test", name: "Eve" },
  ]).returning({ id: user.id });
  [ids.ada, ids.bob, ids.eve] = users.map((row) => row.id) as [string, string, string];
  await harness.db.insert(organization).values({ id: ACME, name: "Acme", slug: "acme" });
  await harness.db.insert(member).values([
    { userId: ids.ada, organizationId: ACME, role: "owner" },
    { userId: ids.bob, organizationId: ACME, role: "admin" },
    { userId: ids.eve, organizationId: ACME, role: "member" },
  ]);
}, 60_000);

afterAll(async () => {
  await harness?.stop();
});

describe("organization tokens", () => {
  beforeEach(async () => {
    await harness.db.delete(organizationToken);
  });

  it("shows a member only their own tokens and lets them revoke only those", async () => {
    const ada = await tokenOf("ada");
    const eve = await tokenOf("eve");
    expect(await listedBy("eve")).toEqual(["eve-ci"]);
    expect(await revokedBy("eve", ada)).toBe("not_found");
    expect(await listedBy("ada")).toContain("ada-ci");
    expect(await revokedBy("eve", eve)).toBe("revoked");
    expect(await listedBy("eve")).toEqual([]);
  });

  it("shows owners and admins every token and lets them revoke any", async () => {
    const ada = await tokenOf("ada");
    await tokenOf("eve");
    expect(await listedBy("bob")).toEqual(["ada-ci", "eve-ci"]);
    expect(await listedBy("ada")).toEqual(["ada-ci", "eve-ci"]);
    expect(await revokedBy("bob", ada)).toBe("revoked");
    expect(await listedBy("ada")).toEqual(["eve-ci"]);
  });
});
