import { Cause, Effect, Exit } from "effect";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { Auth, type AuthService, type AuthSession } from "#/modules/identity/auth.server";
import { user } from "#/modules/identity/tables";
import { member, organization } from "#/modules/organization/tables";
import { SupportStaff, supportStaffOf } from "#/modules/organization/access.server";
import { organizationServerUpgrades } from "#/modules/server-upgrade/tables";
import { organizationSupportAccess } from "#/modules/support-access/tables";
import { handleOrgChangesRequest } from "#/routes/api/org/-changes.handler";
import { Database, type DatabaseService } from "#/server/database.server";
import { startPostgresTestHarness, type PostgresTestHarness } from "#/test/postgres";

const ACME = "00000000-0000-4000-8000-0000000000a1";

let harness: PostgresTestHarness;
let ada: string;
let outsider: string;

beforeAll(async () => {
  harness = await startPostgresTestHarness();
  [ada, outsider] = (await harness.db.insert(user).values([
    { email: "ada@example.test", name: "Ada" },
    { email: "eve@example.test", name: "Eve" },
  ]).returning({ id: user.id })).map((row) => row.id) as [string, string];
  await harness.db.insert(organization).values({ id: ACME, name: "Acme", slug: "acme" });
  await harness.db.insert(member).values({ userId: ada, organizationId: ACME, role: "owner" });
}, 60_000);

afterAll(async () => {
  await harness?.stop();
});

const request = new Request("http://localhost/api/org/changes?organizationSlug=acme");

function signedInAs(userId: string | null) {
  const session = userId === null ? null : asTestDouble<AuthSession>()({ user: { id: userId } });
  return Effect.provideService(Auth, asTestDouble<AuthService>()({ getSession: () => Effect.succeed(session) }));
}

const openStream = (userId: string | null, overrides: Partial<DatabaseService> = {}) => harness.runEffect(
  handleOrgChangesRequest(request, "acme").pipe(
    signedInAs(userId),
    Effect.provideService(SupportStaff, supportStaffOf([])),
    Effect.updateService(Database, (database) => ({ ...database, ...overrides })),
  ),
);

/** Reads a change stream as text. The stream is pulled, so it only polls while a test reads. */
function readEvents(response: Response) {
  if (!response.body) throw new Error("The change stream has no body");
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  const events = {
    text: "",
    /** Reads until `fragment` arrives or the stream ends. */
    async until(fragment: string) {
      while (!events.text.includes(fragment)) {
        const chunk = await reader.read();
        if (chunk.done) break;
        events.text += decoder.decode(chunk.value);
      }
      return events.text;
    },
    cancel: () => reader.cancel(),
  };
  return events;
}

describe("the Organization change stream", () => {
  it.each([
    ["anonymous", null, "Unauthorized"],
    ["non-member", "outsider", "The organization was not found."],
  ])("refuses %s requests", async (_name, who, message) => {
    const exit = await Effect.runPromise(Effect.exit(Effect.promise(() => openStream(who === "outsider" ? outsider : null))));
    expect(Exit.isFailure(exit) && Cause.pretty(exit.cause)).toContain(message);
  });

  it("starts at the current horizon, names the Organization, then the collections that changed after, and disables proxy buffering", async () => {
    await harness.db.insert(organizationSupportAccess).values({ organizationId: ACME, allowed: true });
    const response = await openStream(ada);
    expect(response.headers.get("Content-Type")).toBe("text/event-stream");
    expect(response.headers.get("X-Accel-Buffering")).toBe("no");
    expect(response.headers.get("Cache-Control")).toBe("private, no-store, no-transform");
    const events = readEvents(response);
    await events.until(`data: {"id":"${ACME}"}\n\n`);

    await harness.db.insert(organizationServerUpgrades).values({ organizationId: ACME });
    // The change before the stream opened is the client's refetch on `open`, not an event.
    await events.until('event: changes\ndata: {"collections":["organization_server_upgrades"]}\n\n');
    expect(events.text).not.toContain("organization_support_access");
    expect(events.text).not.toContain("id: ");
    await events.cancel();
    // The cursor is pg_snapshot_xmin, which is server-wide: a transaction open in another test file's database on the
    // same Postgres holds the insert back until it ends.
  }, 30_000);

  it("ends the stream when the change log can't be read, so EventSource reconnects", async () => {
    const real = await harness.runEffect(Database);
    const failing: DatabaseService["drizzle"] = Object.create(real.drizzle, { execute: { value: () => Effect.fail(new Error("database down")) } });
    const response = await openStream(ada, { drizzle: failing });
    // It still opens the stream with `retry:` and ends at once.
    expect(response.status).toBe(200);
    expect(await response.text()).toBe("retry: 1000\n\n");
  });
});
