import { it } from "@effect/vitest";
import type { ConfigCommand, ConfigQuery } from "@ployz/sdk";
import { and, eq } from "drizzle-orm";
import { Effect, Layer } from "effect";
import { expect } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { callCloud } from "#/modules/agent/call-cloud.server";
import { hasApprover } from "#/modules/approvals/approvals.server";
import { createCustomerPortal } from "#/modules/billing/billing.server";
import { gatherDomainEvidence } from "#/modules/config-store/domain-evidence.server";
import { refusal } from "#/modules/config-store/config-store.server";
import { StoreRefused } from "#/modules/config-store/store.contract";
import { startGithubConnect } from "#/modules/github/github-connect.server";
import type { Caller } from "#/modules/identity/actor";
import { type ClaimedAction, claimUrlFor, requireClaimed } from "#/modules/identity/claim-required.server";
import { createOrganizationToken } from "#/modules/identity/organization-token.server";
import { invitation, member, organizationToken, user } from "#/modules/identity/tables";
import { makePloyzLayer } from "#/modules/runtime/ployz.server";
import { createInvite } from "#/modules/organization/invitation.server";
import { handleCliRequest } from "#/routes/api/cli/-cli.handler";
import { handleConfigRequest } from "#/routes/api/config/-config.handler";
import { githubConnectResponse } from "#/routes/api/github/-connect.handler";
import { Auth, AuthLive, type AuthService, type AuthSession } from "#/server/auth.server";
import { Database } from "#/server/database.server";
import { seedStoreGitService, seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000c501";
const ACTIONS: ReadonlyArray<ClaimedAction> = ["add_domain", "mint_token", "invite", "billing", "github_connect"];
const here = { project: null, environment: null };
const domain = (hostname: string | null): ConfigCommand => ({ command: "add_domain", environment: here, service: "web", hostname, port: null });

const noServers = makePloyzLayer({ connect: async () => { throw new Error("Endpoint unavailable"); } });

type CliBody = { name: string; expires_in_days: number };
type Reply = {
  error?: { code: string; message: string; details: { claim_url?: string; action?: string } | null };
  domain?: { kind: string; hostname: string | null };
  domains?: ReadonlyArray<{ kind: string; hostname: string | null }>;
};

const shop = Effect.fn(function* (anonymous: boolean) {
  const cloud = yield* storeTestCloud();
  const services = yield* Layer.build(Layer.mergeAll(AuthLive.pipe(Layer.provide(cloud)), cloud, noServers));
  const provided = <A, E, R>(effect: Effect.Effect<A, E, R>) => effect.pipe(Effect.provide(services));
  const userId = yield* provided(seedStoreOrganization(ORGANIZATION));
  yield* provided(seedStoreGitService(ORGANIZATION, {
    project: "00000000-0000-4000-8000-00000000c502",
    environment: "00000000-0000-4000-8000-00000000c503",
    service: "00000000-0000-4000-8000-00000000c504",
  }));
  const caller: Caller = { userId, organization: { id: ORGANIZATION, slug: "shop" }, credential: { kind: "session", id: "session-1" }, role: "owner", support: null, via: "cli" };
  const { secret } = yield* provided(createOrganizationToken(caller, { name: "agent", expiresInDays: 1 }));
  if (anonymous) {
    yield* provided(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      yield* drizzle.update(user).set({ isAnonymous: true }).where(eq(user.id, userId));
    }));
  }
  const send = (path: string, method: string, body?: ConfigCommand | ConfigQuery | CliBody) => provided(Effect.gen(function* () {
    const init: RequestInit = { method, headers: { "content-type": "application/json", authorization: `Bearer ${secret}` } };
    if (body !== undefined) init.body = JSON.stringify(body);
    const response = yield* (path.startsWith("config/") ? handleConfigRequest : handleCliRequest)(new Request(`http://localhost:3000/api/${path}`, init));
    const reply = response instanceof Response ? response : Response.json(response);
    return { status: reply.status, json: (yield* Effect.promise(() => reply.json())) as Reply };
  }));
  const count = (table: typeof organizationToken | typeof invitation) => provided(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    return (yield* drizzle.select({ id: table.id }).from(table).where(eq(table.organizationId, ORGANIZATION))).length;
  }));
  const domains = () => send("config/read", "POST", { query: "domains", environment: here, service: null })
    .pipe(Effect.map(({ json }) => json.domains?.map(({ kind, hostname }) => ({ kind, hostname }))));
  return { provided, caller, userId, send, count, domains };
});

it.live("an anonymous Organization is refused every claimed action with a 403 that names its claim URL", () =>
  Effect.gen(function* () {
    const { provided } = yield* shop(true);
    const claimUrl = yield* provided(claimUrlFor(ORGANIZATION));
    expect(claimUrl).toMatch(/^http:\/\/localhost:3000\/claim\/[\w-]{43}$/);
    expect(yield* provided(claimUrlFor(ORGANIZATION))).toBe(claimUrl);
    for (const action of ACTIONS) {
      const refused = yield* provided(Effect.flip(requireClaimed(ORGANIZATION, action)));
      if (!(refused instanceof StoreRefused)) return yield* Effect.die(refused);
      expect(refused.refusal).toMatchObject({ code: "claim_required", details: { claim_url: claimUrl, action } });
      expect(refusal(refused.refusal).status).toBe(403);
    }
  }));

it.live("a person's Organization does every claimed action", () =>
  Effect.gen(function* () {
    const { provided, caller, send, domains } = yield* shop(false);
    for (const action of ACTIONS) yield* provided(requireClaimed(ORGANIZATION, action));
    expect(yield* provided(createOrganizationToken(caller, { name: "ci", expiresInDays: 1 }))).toMatchObject({ name: "ci" });
    expect(yield* send("config/write", "POST", domain("shop.example.com")))
      .toMatchObject({ status: 200, json: { domain: { kind: "custom", hostname: "shop.example.com" } } });
    expect(yield* domains()).toEqual([{ kind: "custom", hostname: "shop.example.com" }]);
  }));

it.live("an anonymous Organization's token can't mint another, add a custom domain, or connect GitHub", () =>
  Effect.gen(function* () {
    const { provided, send, count, domains } = yield* shop(true);
    const claimUrl = yield* provided(claimUrlFor(ORGANIZATION));
    const refused = (action: ClaimedAction) => ({ status: 403, json: { error: { code: "claim_required", details: { claim_url: claimUrl, action } } } });
    expect(yield* send("cli/tokens", "POST", { name: "ci", expires_in_days: 1 })).toMatchObject(refused("mint_token"));
    expect(yield* count(organizationToken)).toBe(1);
    expect(yield* send("cli/github", "GET")).toMatchObject(refused("github_connect"));
    expect(yield* send("config/write", "POST", domain("shop.example.com"))).toMatchObject(refused("add_domain"));
    expect(yield* domains()).toEqual([]);
  }));

it.live("an anonymous Organization still adds a generated domain", () =>
  Effect.gen(function* () {
    const { send, domains } = yield* shop(true);
    expect(yield* send("config/write", "POST", domain(null))).toMatchObject({ status: 200, json: { domain: { kind: "generated", hostname: null } } });
    expect(yield* domains()).toEqual([{ kind: "generated", hostname: null }]);
  }));

it.live("an add_domain whose hostname the Store can't read is refused, not waved through", () =>
  Effect.gen(function* () {
    const { provided } = yield* shop(true);
    const unreadable = asTestDouble<ConfigCommand>()({ command: "add_domain", environment: here, service: "web", hostname: 42, port: null });
    const refused = yield* provided(Effect.flip(gatherDomainEvidence(ORGANIZATION, { operation: "write", command: unreadable }, () => Promise.reject(new Error("unread")))));
    expect(refused).toMatchObject({ _tag: "StoreRefused", code: "claim_required" });
  }));

it.live("an Organization with no owner is refused every claimed action and still asks for approval", () =>
  Effect.gen(function* () {
    const { provided } = yield* shop(false);
    yield* provided(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      yield* drizzle.delete(member).where(and(eq(member.organizationId, ORGANIZATION), eq(member.role, "owner")));
    }));
    for (const action of ACTIONS) {
      expect(yield* provided(Effect.flip(requireClaimed(ORGANIZATION, action)))).toMatchObject({ _tag: "StoreRefused", code: "claim_required" });
    }
    expect(yield* provided(hasApprover(ORGANIZATION))).toBe(true);
  }));

it.live("an anonymous Organization's agent asking for GitHub gets the claim URL, not a connect link", () =>
  Effect.gen(function* () {
    const { provided, caller } = yield* shop(true);
    const claimUrl = yield* provided(claimUrlFor(ORGANIZATION));
    expect(yield* provided(callCloud({ ...caller, via: "agent" }, { ask: { query: "github_connection" } }))).toMatchObject({
      ok: false,
      refusal: { code: "claim_required", details: { claim_url: claimUrl, action: "github_connect" } },
    });
  }));

it.live("an anonymous Organization's GitHub connect page links its claim URL", () =>
  Effect.gen(function* () {
    const { provided, userId } = yield* shop(true);
    const claimUrl = yield* provided(claimUrlFor(ORGANIZATION));
    const session = asTestDouble<AuthService>()({ getSession: () => Effect.succeed(asTestDouble<AuthSession>()({ user: { id: userId } })) });
    const page = yield* provided(githubConnectResponse(new Request("http://localhost:3000/api/github/connect?organization=shop")).pipe(
      Effect.provideService(Auth, session),
    ));
    expect(page.status).toBe(403);
    expect(yield* Effect.promise(() => page.text())).toContain(`<a href="${claimUrl}"`);
  }));

it.live("an anonymous Organization can't invite, open the billing portal, or start a GitHub connect", () =>
  Effect.gen(function* () {
    const { provided, userId, count } = yield* shop(true);
    const code = <A, E extends { _tag: string }, R>(effect: Effect.Effect<A, E, R>) =>
      provided(Effect.flip(effect)).pipe(Effect.map((error) => error._tag === "StoreRefused" && "refusal" in error ? (error.refusal as { code: string }).code : error._tag));
    expect(yield* code(createInvite(ORGANIZATION, userId, { email: "bob@example.test", role: "member" }))).toBe("claim_required");
    expect(yield* count(invitation)).toBe(0);
    expect(yield* code(createCustomerPortal({ userId }, { organizationSlug: "shop" }))).toBe("claim_required");
    expect(yield* code(startGithubConnect({ userId }, "shop", false))).toBe("claim_required");
  }));
