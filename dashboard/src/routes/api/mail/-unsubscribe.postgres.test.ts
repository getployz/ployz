import { and, eq } from "drizzle-orm";
import { ConfigProvider, Effect, Layer } from "effect";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { user } from "#/modules/identity/tables";
import type { NotificationType } from "#/modules/notifications/notification-types";
import { getNotificationSettings, setNotificationChannel } from "#/modules/notifications/preferences.server";
import { notificationPreference } from "#/modules/notifications/tables";
import { signUnsubscribeToken } from "#/modules/notifications/unsubscribe-token.server";
import { AppConfig } from "#/server/config.server";
import type { Database } from "#/server/database.server";
import { testConfigEnvironment } from "#/test/config-environment";
import { startPostgresTestHarness, type PostgresTestHarness } from "#/test/postgres";
import { handleUnsubscribeRequest } from "./-unsubscribe.handler";

const SECRET = testConfigEnvironment().APP_ENCRYPTION_SECRET;
const MAIL = { POSTSHIBA_API_KEY: "key", POSTSHIBA_TEAM_ID: "team", POSTSHIBA_CLUSTER_ID: "cluster", MAIL_FROM: "Ployz <mail@example.test>" };

let harness: PostgresTestHarness;
let ada: string;
let bob: string;

const run = <A, E>(effect: Effect.Effect<A, E, Database | AppConfig>, mail: Record<string, string> = {}) => {
  const config = AppConfig.layer.pipe(Layer.provide(ConfigProvider.layer(ConfigProvider.fromEnv({
    env: { ...testConfigEnvironment(), ...mail, DATABASE_URL: harness.databaseUrl },
  }))));
  return harness.runEffect(effect.pipe(Effect.provide(config)));
};

const request = (method: "GET" | "POST", token: string) =>
  run(handleUnsubscribeRequest(new Request(`https://cloud.example.com/api/mail/unsubscribe?token=${encodeURIComponent(token)}`, { method })));

const rowsOf = (userId: string, type: NotificationType) =>
  harness.db.select({ channel: notificationPreference.channel }).from(notificationPreference)
    .where(and(eq(notificationPreference.userId, userId), eq(notificationPreference.type, type)));

beforeAll(async () => {
  harness = await startPostgresTestHarness();
  const users = await harness.db.insert(user).values([
    { email: "ada@example.test", name: "Ada" },
    { email: "bob@example.test", name: "Bob" },
  ]).returning({ id: user.id });
  [ada, bob] = users.map((row) => row.id) as [string, string];
}, 60_000);

afterAll(async () => {
  await harness?.stop();
});

describe("unsubscribe link", () => {
  it("sets the type to None from the footer link and says so", async () => {
    const response = await request("GET", signUnsubscribeToken(SECRET, { userId: ada, type: "uptime" }));

    expect(response.status).toBe(200);
    expect(await response.text()).toContain("You won't get uptime emails anymore.");
    expect(await rowsOf(ada, "uptime")).toEqual([{ channel: "none" }]);
    expect(await rowsOf(ada, "deploy-failed")).toEqual([]);
  });

  it("sets the type to None on a one-click POST, and a repeat keeps one row", async () => {
    const token = signUnsubscribeToken(SECRET, { userId: ada, type: "deploy-failed" });
    for (const _ of [1, 2]) {
      const response = await request("POST", token);
      expect(response.status).toBe(200);
      expect(await response.text()).toBe("");
    }
    expect(await rowsOf(ada, "deploy-failed")).toEqual([{ channel: "none" }]);
  });

  it("writes nothing for a tampered token", async () => {
    const token = signUnsubscribeToken(SECRET, { userId: bob, type: "uptime" });
    const response = await request("GET", `${token.slice(0, -2)}xx`);

    expect(response.status).toBe(400);
    expect(await response.text()).toContain("This link doesn't work");
    expect(await rowsOf(bob, "uptime")).toEqual([]);
  });

  it("answers a deleted user's link and writes nothing", async () => {
    const gone = "00000000-0000-4000-8000-00000000e301";
    const response = await request("POST", signUnsubscribeToken(SECRET, { userId: gone, type: "uptime" }));

    expect(response.status).toBe(200);
    expect(await rowsOf(gone, "uptime")).toEqual([]);
  });
});

describe("notification settings", () => {
  it("defaults every type to Email and reports whether this Cloud can send", async () => {
    const off = await run(getNotificationSettings({ userId: bob }));
    expect(off).toEqual({ mailOn: false, channels: { uptime: "email", "deploy-failed": "email", "upgrade-failed": "email" } });
    expect((await run(getNotificationSettings({ userId: bob }), MAIL)).mailOn).toBe(true);
  });

  it("saves a change over an earlier choice", async () => {
    await run(setNotificationChannel(bob, "upgrade-failed", "none"));
    await run(setNotificationChannel(bob, "upgrade-failed", "email"));
    await run(setNotificationChannel(bob, "uptime", "none"));

    expect((await run(getNotificationSettings({ userId: bob }))).channels)
      .toEqual({ uptime: "none", "deploy-failed": "email", "upgrade-failed": "email" });
    expect(await rowsOf(bob, "upgrade-failed")).toEqual([{ channel: "email" }]);
  });
});
