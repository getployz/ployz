import { ConfigProvider, Effect, Layer } from "effect";
import { Inngest } from "inngest";
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import type { JsonValue } from "@ployz/sdk";
import { asTestDouble } from "#/lib/test-double";
import type { Caller } from "#/modules/identity/actor";
import { member, user } from "#/modules/identity/tables";
import { InngestClient } from "#/modules/inngest/client";
import { orgMemberInvite } from "#/modules/organization/members.commands.server";
import { organization } from "#/modules/organization/tables";
import { Auth, type AuthService } from "#/server/auth.server";
import { AppConfig } from "#/server/config.server";
import type { Database } from "#/server/database.server";
import { testConfigEnvironment } from "#/test/config-environment";
import { startPostgresTestHarness, type PostgresTestHarness } from "#/test/postgres";

const ACME = "00000000-0000-4000-8000-00000000c0e1";
const MAIL = {
  POSTSHIBA_API_KEY: "postshiba-api-key",
  POSTSHIBA_TEAM_ID: "team-1",
  POSTSHIBA_CLUSTER_ID: "cluster-1",
  MAIL_FROM: "Ployz <noreply@example.test>",
};

const inngest = new Inngest({ id: "invitation-mail-test" });
const send = vi.spyOn(inngest, "send");
const noSession = asTestDouble<AuthService>()({ getSession: () => Effect.succeed(null) });
const BASE = { ...testConfigEnvironment(), NODE_ENV: "test", DATABASE_URL: "postgres://unused@localhost/unused" };
const configOf = (mail: boolean) => AppConfig.layer.pipe(Layer.provide(ConfigProvider.layer(ConfigProvider.fromEnv({
  env: mail ? { ...BASE, ...MAIL } : BASE,
}))));

let harness: PostgresTestHarness;
let ada = "";

type Invited = { id: string; email: string | null; link: string; emailed_to: string | null };

const invite = async (input: JsonValue, mail: boolean) => {
  const caller: Caller = {
    userId: ada,
    organization: { id: ACME, slug: "acme" },
    credential: { kind: "session", id: "ada-session" },
    role: "owner",
    support: null,
    via: "dashboard",
  };
  const command: { invoke: (caller: Caller, raw: JsonValue, call: { approvalId: null; key: null }) => Effect.Effect<
    { ok: true; output: unknown; message?: string } | { ok: false; refusal: { message: string } },
    unknown,
    Database | InngestClient | Auth | AppConfig
  > } = orgMemberInvite;
  const result = await harness.runEffect(command.invoke(caller, input, { approvalId: null, key: null }).pipe(
    Effect.provideService(InngestClient, inngest),
    Effect.provideService(Auth, noSession),
    Effect.provide(configOf(mail)),
  ));
  if (!result.ok) throw new Error(result.refusal.message);
  return { ...(result.output as Invited), message: result.message };
};

beforeAll(async () => {
  harness = await startPostgresTestHarness();
  const [row] = await harness.db.insert(user).values({ email: "ada@example.test", name: "Ada <Lovelace>" }).returning({ id: user.id });
  ada = row?.id ?? "";
  await harness.db.insert(organization).values({ id: ACME, name: "Acme & Co", slug: "acme" });
  await harness.db.insert(member).values({ userId: ada, organizationId: ACME, role: "owner" });
}, 60_000);

afterAll(async () => {
  await harness?.stop();
});

beforeEach(() => {
  send.mockReset();
  send.mockResolvedValue({ ids: [] });
});

describe("invite email", () => {
  it("queues one mail to the labelled address, keyed by the invitation, carrying the link", async () => {
    const invited = await invite({ email: "Bob@Example.test", role: "admin" }, true);
    expect(invited.emailed_to).toBe("bob@example.test");
    expect(invited.message).toBe("Emailed to bob@example.test. The link works once and lasts 7 days.");
    expect(send).toHaveBeenCalledTimes(1);
    const event = send.mock.calls[0]?.[0] as { id: string; name: string; data: { to: string; subject: string; text: string; html: string } };
    expect(event.id).toBe(`mail-invitation-${invited.id}`);
    expect(event.name).toBe("mail/send");
    expect(event.data.to).toBe("bob@example.test");
    expect(event.data.subject).toBe("Ada <Lovelace> invited you to Acme & Co on Ployz");
    expect(event.data.text).toContain(`Join Acme & Co: ${invited.link}`);
    expect(event.data.text).toContain("as an admin");
    expect(event.data.html).toContain(`href="${invited.link}"`);
    expect(event.data.html).toContain("/assets/logo-mark.png");
    expect(event.data.html).toContain("Ada &lt;Lovelace&gt;");
    expect(event.data.html).not.toContain("<Lovelace>");
  });

  it("queues nothing when Cloud has no mail server, and says to send the link", async () => {
    const invited = await invite({ email: "carol@example.test" }, false);
    expect(invited.emailed_to).toBeNull();
    expect(invited.message).toBe("Send this link to the person you're inviting. It works once and lasts 7 days.");
    expect(send).not.toHaveBeenCalled();
  });

  it("refuses more than one address, so a mailed invite has one recipient", async () => {
    for (const email of ["a@example.test, b@example.test", "a@example.test;b@example.test", "Bob <b@example.test>", "a@example.test b@example.test", "bob@example.test bob", "bob@example", "bob"]) {
      await expect(invite({ email }, true), email).rejects.toThrow("Enter one email address");
    }
    expect(send).not.toHaveBeenCalled();
  });

  it("queues nothing for an invite with no address", async () => {
    const invited = await invite({}, true);
    expect(invited.emailed_to).toBeNull();
    expect(send).not.toHaveBeenCalled();
  });

  it("still returns the link when the mail cannot be queued", async () => {
    send.mockRejectedValue(new Error("inngest down"));
    const invited = await invite({ email: "dan@example.test" }, true);
    expect(invited.emailed_to).toBeNull();
    expect(invited.link).toContain("/invite/");
  });
});

