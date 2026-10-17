import { it } from "@effect/vitest";
import type { ConfigCommand } from "@ployz/sdk";
import { eq } from "drizzle-orm";
import { Effect, Layer } from "effect";
import { expect } from "vitest";
import { createCustomerPortal } from "#/modules/billing/billing.server";
import { refusal } from "#/modules/config-store/config-store.server";
import { StoreRefused } from "#/modules/config-store/store.contract";
import { startGithubConnect } from "#/modules/github/github-connect.server";
import type { Caller } from "#/modules/identity/actor";
import { type ClaimedAction, claimUrlFor, requireClaimed } from "#/modules/identity/claim-required.server";
import { createOrganizationToken } from "#/modules/identity/organization-token.server";
import { user } from "#/modules/identity/tables";
import { makePloyzLayer } from "#/modules/runtime/ployz.server";
import { createInvite } from "#/modules/organization/invitation.server";
import { handleCliRequest } from "#/routes/api/cli/-cli.handler";
import { handleConfigRequest } from "#/routes/api/config/-config.handler";
import { AuthLive } from "#/server/auth.server";
import { Database } from "#/server/database.server";
import { seedStoreGitService, seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000c501";
const ACTIONS: ReadonlyArray<ClaimedAction> = ["add_domain", "mint_token", "invite", "billing", "github_connect"];
const here = { project: null, environment: null };
const domain = (hostname: string | null): ConfigCommand => ({ command: "add_domain", environment: here, service: "web", hostname, port: null });

/** No Server answers: no claimed action reaches one. */
const noServers = makePloyzLayer({ connect: async () => { throw new Error("Endpoint unavailable"); } });

type CliBody = { name: string; expires_in_days: number };
type Reply = { error?: { code: string; message: string; details: { claim_url?: string; action?: string } | null } };

/** Shop, with a token its owner minted, owned by an anonymous agent when `anonymous` holds. */
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
  const send = (path: string, method: string, body?: ConfigCommand | CliBody) => provided(Effect.gen(function* () {
    const init: RequestInit = { method, headers: { "content-type": "application/json", authorization: `Bearer ${secret}` } };
    if (body !== undefined) init.body = JSON.stringify(body);
    const response = yield* (path.startsWith("config/") ? handleConfigRequest : handleCliRequest)(new Request(`http://localhost:3000/api/${path}`, init));
    const reply = response instanceof Response ? response : Response.json(response);
    return { status: reply.status, json: (yield* Effect.promise(() => reply.json())) as Reply };
  }));
  return { provided, caller, userId, send };
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
    const { provided, caller, send } = yield* shop(false);
    for (const action of ACTIONS) yield* provided(requireClaimed(ORGANIZATION, action));
    expect(yield* provided(createOrganizationToken(caller, { name: "ci", expiresInDays: 1 }))).toMatchObject({ name: "ci" });
    expect((yield* send("config/write", "POST", domain("shop.example.com"))).json.error?.code).not.toBe("claim_required");
  }));

it.live("an anonymous Organization's token can't mint another, add a custom domain, or connect GitHub", () =>
  Effect.gen(function* () {
    const { provided, send } = yield* shop(true);
    const claimUrl = yield* provided(claimUrlFor(ORGANIZATION));
    const refused = (action: ClaimedAction) => ({ status: 403, json: { error: { code: "claim_required", details: { claim_url: claimUrl, action } } } });
    expect(yield* send("cli/tokens", "POST", { name: "ci", expires_in_days: 1 })).toMatchObject(refused("mint_token"));
    expect(yield* send("cli/github", "GET")).toMatchObject(refused("github_connect"));
    expect(yield* send("config/write", "POST", domain("shop.example.com"))).toMatchObject(refused("add_domain"));
  }));

it.live("an anonymous Organization still adds a generated domain", () =>
  Effect.gen(function* () {
    const { send } = yield* shop(true);
    expect((yield* send("config/write", "POST", domain(null))).json.error?.code).not.toBe("claim_required");
  }));

it.live("an anonymous Organization can't invite, open the billing portal, or start a GitHub connect", () =>
  Effect.gen(function* () {
    const { provided, userId } = yield* shop(true);
    const code = <A, E extends { _tag: string }, R>(effect: Effect.Effect<A, E, R>) =>
      provided(Effect.flip(effect)).pipe(Effect.map((error) => error._tag === "StoreRefused" && "refusal" in error ? (error.refusal as { code: string }).code : error._tag));
    expect(yield* code(createInvite(ORGANIZATION, userId, { email: "bob@example.test", role: "member" }))).toBe("claim_required");
    expect(yield* code(createCustomerPortal({ userId }, { organizationSlug: "shop" }))).toBe("claim_required");
    expect(yield* code(startGithubConnect({ userId }, "shop", false))).toBe("claim_required");
  }));
