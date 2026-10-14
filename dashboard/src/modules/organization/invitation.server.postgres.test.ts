import { and, eq } from "drizzle-orm";
import { ConfigProvider, Effect, Layer } from "effect";
import { Inngest } from "inngest";
import { afterAll, beforeAll, describe, expect, it, vi } from "vitest";
import type { JsonValue } from "@ployz/sdk";
import { asTestDouble } from "#/lib/test-double";
import type { Caller } from "#/modules/identity/actor";
import { invitation, member, user } from "#/modules/identity/tables";
import { InngestClient } from "#/modules/inngest/client";
import { acceptInvite, readInvite, revokeInvite } from "#/modules/organization/invitation.server";
import { orgInviteLs, orgInviteRevoke, orgLeave, orgMemberInvite, orgMemberRm, orgMemberRole } from "#/modules/organization/members.commands.server";
import { organization } from "#/modules/organization/tables";
import { Auth, type AuthService } from "#/server/auth.server";
import { AppConfig } from "#/server/config.server";
import type { Database } from "#/server/database.server";
import { testConfigEnvironment } from "#/test/config-environment";
import { startPostgresTestHarness, type PostgresTestHarness } from "#/test/postgres";

const ACME = "00000000-0000-4000-8000-00000000c001";

const inngest = new Inngest({ id: "invitation-test" });
vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });
const noSession = asTestDouble<AuthService>()({ getSession: () => Effect.succeed(null) });
const config = AppConfig.layer.pipe(Layer.provide(ConfigProvider.layer(ConfigProvider.fromEnv({ env: { ...testConfigEnvironment(), NODE_ENV: "test", DATABASE_URL: "postgres://unused@localhost/unused" } }))));

let harness: PostgresTestHarness;
const ids = { ada: "", bob: "", carol: "", dan: "", eve: "" };

const run = <A, E>(effect: Effect.Effect<A, E, Database | InngestClient | Auth | AppConfig>) =>
  harness.runEffect(effect.pipe(Effect.provideService(InngestClient, inngest), Effect.provideService(Auth, noSession), Effect.provide(config)));

const roleOf = async (userId: string) => (await harness.db.select({ role: member.role }).from(member)
  .where(and(eq(member.organizationId, ACME), eq(member.userId, userId))))[0]?.role ?? null;

const COMMANDS = {
  org_member_invite: orgMemberInvite,
  org_member_role: orgMemberRole,
  org_member_rm: orgMemberRm,
  org_invite_ls: orgInviteLs,
  org_invite_revoke: orgInviteRevoke,
  org_leave: orgLeave,
};

/** Runs a member command as `who`, holding whatever role they have in Acme now. */
const as = async (who: keyof typeof ids, name: keyof typeof COMMANDS, input: JsonValue) => {
  const caller: Caller = {
    userId: ids[who],
    organization: { id: ACME, slug: "acme" },
    credential: { kind: "session", id: `${who}-session` },
    role: await roleOf(ids[who]),
    support: null,
    via: "dashboard",
  };
  const command: { invoke: (caller: Caller, raw: JsonValue, call: { approvalId: null; key: null }) => Effect.Effect<
    { ok: true; output: unknown } | { ok: false; refusal: { code: string; message: string } },
    unknown,
    Database | InngestClient | Auth | AppConfig
  > } = COMMANDS[name];
  return run(command.invoke(caller, input, { approvalId: null, key: null }));
};

const outcome = (effect: Effect.Effect<unknown, { readonly _tag: string }, Database | InngestClient | Auth | AppConfig>) =>
  run(effect.pipe(Effect.as("ok"), Effect.catch((error) => Effect.succeed(error._tag))));

const tokenOf = (link: string) => new URL(link).pathname.split("/").at(-1) ?? "";

const invite = async (who: keyof typeof ids, input: JsonValue) => {
  const result = await as(who, "org_member_invite", input);
  if (!result.ok) throw new Error(result.refusal.message);
  return result.output as { id: string; link: string };
};

beforeAll(async () => {
  harness = await startPostgresTestHarness();
  const users = await harness.db.insert(user).values([
    { email: "ada@example.test", name: "Ada" },
    { email: "bob@example.test", name: "Bob" },
    { email: "carol@example.test", name: "Carol" },
    { email: "dan@example.test", name: "Dan" },
    { email: "eve@example.test", name: "Eve" },
  ]).returning({ id: user.id });
  [ids.ada, ids.bob, ids.carol, ids.dan, ids.eve] = users.map((row) => row.id) as [string, string, string, string, string];
  await harness.db.insert(organization).values({ id: ACME, name: "Acme", slug: "acme" });
  await harness.db.insert(member).values([
    { userId: ids.ada, organizationId: ACME, role: "owner" },
    { userId: ids.carol, organizationId: ACME, role: "admin" },
  ]);
}, 60_000);

afterAll(async () => {
  await harness?.stop();
});

describe("invite links", () => {
  it("lets whoever opens the link join once with the role it names, and no one after", async () => {
    const { link } = await invite("ada", { role: "admin" });
    expect(link.startsWith("http://localhost:3000/invite/")).toBe(true);
    const token = tokenOf(link);
    expect(await run(readInvite(token))).toMatchObject({ organization: "Acme", slug: "acme", invitedBy: "Ada", role: "admin", state: "pending" });

    expect(await run(acceptInvite(token, ids.bob))).toEqual({ slug: "acme" });
    expect(await roleOf(ids.bob)).toBe("admin");
    expect((await run(readInvite(token)))?.state).toBe("accepted");
    expect(await outcome(acceptInvite(token, ids.dan))).toBe("Conflict");
    expect(await roleOf(ids.dan)).toBeNull();
  });

  it("keeps only a hash of the token, so the stored row cannot be replayed as a link", async () => {
    const { id, link } = await invite("ada", {});
    const [row] = await harness.db.select({ tokenHash: invitation.tokenHash }).from(invitation).where(eq(invitation.id, id));
    expect(row?.tokenHash).not.toContain(tokenOf(link));
    expect(await run(readInvite(row?.tokenHash ?? ""))).toBeNull();
  });

  it("refuses an expired link and a revoked one", async () => {
    const expired = await invite("ada", { email: "dan@example.test" });
    await harness.db.update(invitation).set({ expiresAt: new Date(Date.now() - 1000) }).where(eq(invitation.id, expired.id));
    expect((await run(readInvite(tokenOf(expired.link))))?.state).toBe("expired");
    expect(await outcome(acceptInvite(tokenOf(expired.link), ids.dan))).toBe("Conflict");

    const revoked = await invite("carol", { email: "dan@example.test" });
    const pendingIds = async () => {
      const listed = await as("carol", "org_invite_ls", {});
      return listed.ok ? (listed.output as Array<{ id: string }>).map((row) => row.id) : [];
    };
    expect(await as("carol", "org_invite_ls", {})).toMatchObject({
      ok: true,
      output: expect.arrayContaining([expect.objectContaining({ id: revoked.id, email: "dan@example.test", invited_by: "Carol" })]),
    });
    expect(await pendingIds()).not.toContain(expired.id);
    expect(await as("carol", "org_invite_revoke", { invite: "dan@example.test" })).toMatchObject({ ok: true });
    expect((await run(readInvite(tokenOf(revoked.link))))?.state).toBe("revoked");
    expect(await outcome(acceptInvite(tokenOf(revoked.link), ids.dan))).toBe("Conflict");
    expect(await roleOf(ids.dan)).toBeNull();
    expect(await pendingIds()).not.toContain(revoked.id);
  });

  it("refuses to revoke a link someone joins through while the revoke runs", async () => {
    const { id, link } = await invite("ada", {});
    const { revoke } = await harness.db.transaction(async (tx) => {
      await tx.select({ id: invitation.id }).from(invitation).where(eq(invitation.id, id)).for("update");
      const revoke = outcome(revokeInvite(ACME, id));
      await new Promise((resolve) => setTimeout(resolve, 300));
      await tx.update(invitation).set({ status: "accepted", acceptedByUserId: ids.bob }).where(eq(invitation.id, id));
      return { revoke };
    });
    expect(await revoke).toBe("Conflict");
    expect((await run(readInvite(tokenOf(link))))?.state).toBe("accepted");
  });

  it("refuses a link whose sender stops being an admin while someone joins through it", async () => {
    const { link } = await invite("carol", {});
    const { joined } = await harness.db.transaction(async (tx) => {
      await tx.update(member).set({ role: "member" }).where(and(eq(member.organizationId, ACME), eq(member.userId, ids.carol)));
      const joined = outcome(acceptInvite(tokenOf(link), ids.dan));
      await new Promise((resolve) => setTimeout(resolve, 300));
      return { joined };
    });
    expect(await joined).toBe("Conflict");
    expect(await roleOf(ids.dan)).toBeNull();
    await harness.db.update(member).set({ role: "admin" }).where(and(eq(member.organizationId, ACME), eq(member.userId, ids.carol)));
  });

  it("shows a link from a sender who is no longer an admin as revoked, and lets someone else invite that email again", async () => {
    const sent = await invite("carol", { email: "dan@example.test" });
    await harness.db.update(member).set({ role: "member" }).where(and(eq(member.organizationId, ACME), eq(member.userId, ids.carol)));
    expect((await run(readInvite(tokenOf(sent.link))))?.state).toBe("revoked");
    const listed = await as("ada", "org_invite_ls", {});
    expect(listed.ok ? (listed.output as Array<{ id: string }>).map((row) => row.id) : null).not.toContain(sent.id);
    const again = await invite("ada", { email: "dan@example.test" });
    expect(await as("ada", "org_invite_revoke", { invite: "dan@example.test" })).toMatchObject({ ok: true, output: { id: again.id } });
    await harness.db.update(member).set({ role: "admin" }).where(and(eq(member.organizationId, ACME), eq(member.userId, ids.carol)));
  });
});

describe("member commands", () => {
  it("refuses a member who invites, changes a role, or removes someone", async () => {
    const { link } = await invite("ada", {});
    await run(acceptInvite(tokenOf(link), ids.eve));
    expect(await roleOf(ids.eve)).toBe("member");
    expect(await as("eve", "org_member_invite", {})).toMatchObject({ ok: false, refusal: { code: "forbidden" } });
    expect(await as("eve", "org_member_role", { member: "carol@example.test", role: "member" })).toMatchObject({ ok: false, refusal: { code: "forbidden" } });
    expect(await as("eve", "org_member_rm", { member: "Carol" })).toMatchObject({ ok: false, refusal: { code: "forbidden" } });
    expect(await roleOf(ids.carol)).toBe("admin");
  });

  it("refuses an admin who makes an owner or touches one, and lets an owner do it", async () => {
    expect(await as("carol", "org_member_role", { member: "eve@example.test", role: "owner" })).toMatchObject({ ok: false, refusal: { code: "forbidden" } });
    expect(await as("carol", "org_member_rm", { member: "ada@example.test" })).toMatchObject({ ok: false, refusal: { code: "forbidden" } });
    expect(await roleOf(ids.eve)).toBe("member");
    expect(await as("carol", "org_member_role", { member: "Eve", role: "admin" })).toMatchObject({ ok: true, output: { email: "eve@example.test", role: "admin" } });
    expect(await as("ada", "org_member_role", { member: "Eve", role: "owner" })).toMatchObject({ ok: true });
    expect(await roleOf(ids.eve)).toBe("owner");
  });

  it("refuses the last owner who leaves or steps down, and lets them once another owner exists", async () => {
    expect(await as("ada", "org_member_rm", { member: "eve@example.test" })).toMatchObject({ ok: true });
    expect(await as("ada", "org_leave", {})).toMatchObject({ ok: false, refusal: { code: "conflict" } });
    expect(await as("ada", "org_member_role", { member: "ada@example.test", role: "admin" })).toMatchObject({ ok: false, refusal: { code: "conflict" } });
    expect(await roleOf(ids.ada)).toBe("owner");

    expect(await as("ada", "org_member_role", { member: "Carol", role: "owner" })).toMatchObject({ ok: true });
    expect(await as("ada", "org_leave", {})).toMatchObject({ ok: true });
    expect(await roleOf(ids.ada)).toBeNull();
  });

  it("asks which one when two members share a name", async () => {
    await harness.db.update(user).set({ name: "Sam" }).where(eq(user.id, ids.bob));
    await harness.db.update(user).set({ name: "Sam" }).where(eq(user.id, ids.carol));
    expect(await as("carol", "org_member_rm", { member: "Sam" })).toMatchObject({
      ok: false,
      refusal: { code: "ambiguous" },
    });
    expect(await roleOf(ids.bob)).toBe("admin");
  });

  it("reads a selector with an @ as an email only, never as a name", async () => {
    await harness.db.update(user).set({ name: "dan@example.test" }).where(eq(user.id, ids.bob));
    expect(await as("carol", "org_member_rm", { member: "dan@example.test" })).toMatchObject({ ok: false, refusal: { code: "not_found" } });
    expect(await roleOf(ids.bob)).toBe("admin");
  });
});
