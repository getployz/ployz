import { Effect } from "effect";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { member, user } from "#/modules/identity/tables";
import { requireAccess, SupportStaff, supportStaffOf } from "#/modules/organization/access.server";
import { organization } from "#/modules/organization/tables";
import { setSupportAccess } from "#/modules/support-access/support-access.server";
import type { Database } from "#/server/database.server";
import { startPostgresTestHarness, type PostgresTestHarness } from "#/test/postgres";
import type { SecretEncryption } from "#/utils/encrypted-secret.server";

const ORGANIZATION = "00000000-0000-4000-8000-00000000a001";
const SLUG = "acme";

let harness: PostgresTestHarness;
let owner: string;
let staff: string;

const run = <A, E>(effect: Effect.Effect<A, E, Database | SecretEncryption>) =>
  harness.runEffect(effect.pipe(Effect.provideService(SupportStaff, supportStaffOf([staff]))));

const staffAccess = () => run(requireAccess({ userId: staff }, SLUG, "operate").pipe(
  Effect.map((access) => access.kind),
  Effect.catchTag("NotFound", () => Effect.succeed("not_found")),
));

const switchTo = (allowed: boolean) => run(setSupportAccess({ userId: owner }, { organizationSlug: SLUG, allowed }));

beforeAll(async () => {
  harness = await startPostgresTestHarness();
  const users = await harness.db.insert(user).values([
    { email: "owner@example.test", name: "Owner" },
    { email: "staff@example.test", name: "Staff" },
  ]).returning({ id: user.id });
  [owner, staff] = users.map((row) => row.id) as [string, string];
  await harness.db.insert(organization).values({ id: ORGANIZATION, name: "Acme", slug: SLUG });
  await harness.db.insert(member).values({ userId: owner, organizationId: ORGANIZATION, role: "owner" });
}, 60_000);

afterAll(async () => {
  await harness?.stop();
});

describe("support access", () => {
  it("is off by default and lets staff in only while the owner allows it", async () => {
    expect(await staffAccess()).toBe("not_found");
    await switchTo(true);
    expect(await staffAccess()).toBe("support");
    await switchTo(false);
    expect(await staffAccess()).toBe("not_found");
  });
});
