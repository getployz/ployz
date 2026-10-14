import { ConfigProvider, Effect, Layer, Option } from "effect";
import { Inngest } from "inngest";
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { InngestClient } from "#/modules/inngest/client";
import { member, user } from "#/modules/identity/tables";
import type { NotificationType } from "#/modules/notifications/notification-types";
import { queueOrganizationMail, resolveRecipients } from "#/modules/notifications/recipients.server";
import { notificationPreference } from "#/modules/notifications/tables";
import { verifyUnsubscribeToken } from "#/modules/notifications/unsubscribe-token.server";
import { organization } from "#/modules/organization/tables";
import { AppConfig } from "#/server/config.server";
import type { Database } from "#/server/database.server";
import { testConfigEnvironment } from "#/test/config-environment";
import { startPostgresTestHarness, type PostgresTestHarness } from "#/test/postgres";

const ACME = "00000000-0000-4000-8000-00000000e201";
const GLOBEX = "00000000-0000-4000-8000-00000000e202";
const SECRET = testConfigEnvironment().APP_ENCRYPTION_SECRET;

let harness: PostgresTestHarness;
let ada: string;
let bob: string;
let cy: string;
let outsider: string;

const inngest = new Inngest({ id: "recipients-test" });
const send = vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });

const run = <A, E>(effect: Effect.Effect<A, E, Database | AppConfig | InngestClient>) => {
  const config = AppConfig.layer.pipe(Layer.provide(ConfigProvider.layer(ConfigProvider.fromEnv({
    env: { ...testConfigEnvironment(), APP_URL: "https://cloud.example.com", DATABASE_URL: harness.databaseUrl },
  }))));
  return harness.runEffect(effect.pipe(Effect.provide(config), Effect.provideService(InngestClient, inngest)));
};

const recipientsOf = async (type: NotificationType) =>
  (await run(resolveRecipients(ACME, type))).map((recipient) => recipient.email).sort();

const content = {
  subject: "api is down",
  preview: "api stopped answering.",
  title: "api is down",
  paragraphs: ["api stopped answering."],
  action: { label: "Open incident", url: "https://cloud.example.com/incident" },
  reason: "You get this because you are a member of Acme.",
};

beforeAll(async () => {
  harness = await startPostgresTestHarness();
  const users = await harness.db.insert(user).values([
    { email: "ada@acme.test", name: "Ada" },
    { email: "bob@acme.test", name: "Bob" },
    { email: "cy@acme.test", name: "Cy" },
    { email: "out@globex.test", name: "Out" },
  ]).returning({ id: user.id });
  [ada, bob, cy, outsider] = users.map((row) => row.id) as [string, string, string, string];
  await harness.db.insert(organization).values([
    { id: ACME, name: "Acme", slug: "acme" },
    { id: GLOBEX, name: "Globex", slug: "globex" },
  ]);
  await harness.db.insert(member).values([
    { userId: ada, organizationId: ACME, role: "owner" },
    { userId: bob, organizationId: ACME, role: "member" },
    { userId: cy, organizationId: ACME, role: "member" },
    { userId: outsider, organizationId: GLOBEX, role: "owner" },
  ]);
  await harness.db.insert(notificationPreference).values([
    { userId: bob, type: "uptime", channel: "none" },
    { userId: cy, type: "deploy-failed", channel: "none" },
    { userId: cy, type: "uptime", channel: "email" },
  ]);
}, 60_000);

afterAll(async () => {
  await harness?.stop();
});

beforeEach(() => {
  send.mockClear();
});

describe("resolveRecipients", () => {
  it("leaves out a member who set the type to None and keeps one with no preference", async () => {
    expect(await recipientsOf("uptime")).toEqual(["ada@acme.test", "cy@acme.test"]);
  });

  it("reads None only for the type it was set on", async () => {
    expect(await recipientsOf("deploy-failed")).toEqual(["ada@acme.test", "bob@acme.test"]);
    expect(await recipientsOf("upgrade-failed")).toEqual(["ada@acme.test", "bob@acme.test", "cy@acme.test"]);
  });
});

describe("queueOrganizationMail", () => {
  it("queues one mail per recipient, keyed per recipient, each with its own unsubscribe link", async () => {
    const queued = await run(queueOrganizationMail({ organizationId: ACME, type: "uptime", key: "uptime-incident-1", content }));

    expect(queued).toBe(2);
    const events = send.mock.calls.map(([event]) => event).flat();
    expect(Object.fromEntries(events.map((event) => [event.data["to"], event.id]))).toEqual({
      "ada@acme.test": `mail-uptime-incident-1:${ada}`,
      "cy@acme.test": `mail-uptime-incident-1:${cy}`,
    });
    expect(events).toHaveLength(2);

    for (const event of events) {
      const headers = event.data["headers"] as Record<string, string>;
      expect(headers["List-Unsubscribe-Post"]).toBe("List-Unsubscribe=One-Click");
      const link = /^<(https:\/\/cloud\.example\.com\/api\/mail\/unsubscribe\?token=(.+))>$/.exec(headers["List-Unsubscribe"] ?? "");
      expect(link).not.toBeNull();
      const [, url = "", token = ""] = link ?? [];
      const to = event.data["to"] === "ada@acme.test" ? ada : cy;
      expect(verifyUnsubscribeToken(SECRET, token)).toEqual(Option.some({ userId: to, type: "uptime" }));
      expect(event.data["text"]).toContain(`Turn off uptime emails: ${url}`);
      expect(event.data["html"]).toContain(url);
    }
  });

  it("queues nothing when every member set the type to None", async () => {
    await harness.db.insert(notificationPreference).values({ userId: outsider, type: "uptime", channel: "none" });

    expect(await run(queueOrganizationMail({ organizationId: GLOBEX, type: "uptime", key: "uptime-incident-2", content }))).toBe(0);
    expect(send).not.toHaveBeenCalled();
  });
});
