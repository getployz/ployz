import { randomUUID } from "node:crypto";
import { afterAll, beforeAll, expect, it } from "vitest";
import { session, user } from "#/modules/identity/tables";
import { startPostgresTestHarness, type PostgresTestHarness } from "#/test/postgres";
import { listAccountSessions, revokeAccountSession } from "./account.server";

let harness: PostgresTestHarness;
let ada: string;
let bob: string;

const addSession = async (userId: string, userAgent: string | null) => {
  const rows = await harness.db.insert(session).values({
    userId, userAgent, token: randomUUID(), expiresAt: new Date(Date.now() + 86_400_000),
  }).returning({ id: session.id });
  return rows.map((row) => row.id)[0] as string;
};

const remaining = async (userId: string) =>
  (await harness.runEffect(listAccountSessions({ userId }))).map((row) => row.id).sort();

beforeAll(async () => {
  harness = await startPostgresTestHarness();
  [ada, bob] = (await harness.db.insert(user).values([
    { email: "ada@example.test", name: "Ada" },
    { email: "bob@example.test", name: "Bob" },
  ]).returning({ id: user.id })).map((row) => row.id) as [string, string];
}, 60_000);

afterAll(async () => {
  await harness?.stop();
});

it("signs out a browser session and one with no user agent, but never a CLI session or another user's", async () => {
  const browser = await addSession(ada, "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0) Chrome/130.0");
  const unknown = await addSession(ada, null);
  const cli = await addSession(ada, "ployz-cli/0.2.4");
  const bobs = await addSession(bob, "Mozilla/5.0 (X11; Linux x86_64) Firefox/131.0");

  for (const id of [browser, unknown, cli, bobs]) await harness.runEffect(revokeAccountSession({ userId: ada }, id));

  expect(await remaining(ada)).toEqual([cli]);
  expect(await remaining(bob)).toEqual([bobs]);
});
