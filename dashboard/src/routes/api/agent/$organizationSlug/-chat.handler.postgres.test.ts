import { it } from "@effect/vitest";
import { ConfigProvider, Effect, Layer } from "effect";
import { expect } from "vitest";
import { agentPersistence } from "#/modules/agent/persistence.server";
import { resolveCaller } from "#/modules/identity/caller.server";
import { createOrganizationToken } from "#/modules/identity/organization-token.server";
import { member } from "#/modules/identity/tables";
import { handleAgentChat } from "#/routes/api/agent/$organizationSlug/-chat.handler";
import { Auth, AuthLive } from "#/server/auth.server";
import { Database } from "#/server/database.server";
import { encodePublicError, statusForPublicError } from "#/server/public-error";
import { storeTestCloud } from "#/test/store-cloud";

const origin = "http://localhost:3000";

/** A browser session for `name`, active in their personal Organization. */
const signUp = Effect.fn(function* (name: string) {
  const auth = yield* Auth;
  const response = yield* auth.handler(new Request(`${origin}/api/auth/sign-up/email`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ email: `${name}@example.test`, name, password: "correct-horse-battery-staple" }),
  }));
  expect(response.status).toBe(200);
  const cookie = response.headers.get("set-cookie")?.split(";", 1)[0] ?? expect.fail("no session cookie");
  const caller = yield* resolveCaller(new Headers({ cookie }));
  return { cookie, caller };
});

/** What the sidebar posts for one turn, and the status and stream it gets back. */
const reply = Effect.fn(function* (slug: string, headers: Readonly<Record<string, string>>, threadId: string) {
  const request = new Request(`${origin}/api/agent/${slug}/chat`, {
    method: "POST",
    headers: { ...headers, "content-type": "application/json" },
    body: JSON.stringify({ threadId, runId: crypto.randomUUID(), messages: [{ id: crypto.randomUUID(), role: "user", content: "hello" }], tools: [], context: [] }),
  });
  return yield* handleAgentChat(request, slug).pipe(
    Effect.flatMap((response) => Effect.promise(() => response.text()).pipe(Effect.map((body) => ({ status: response.status, body })))),
    Effect.catch((error) => Effect.succeed({ status: statusForPublicError(encodePublicError(error)), body: "" })),
  );
});

const chat = (slug: string, headers: Readonly<Record<string, string>>, threadId: string) =>
  reply(slug, headers, threadId).pipe(Effect.map(({ status }) => status));

/** `body` against a fresh Cloud whose sidebar reads `env`: the stub model unless a test says otherwise. */
const inCloud = <A, E, R>(body: Effect.Effect<A, E, R>, env: Record<string, string> = { PLOYZ_AGENT_STUB: "1" }) => Effect.gen(function* () {
  const cloud = yield* storeTestCloud();
  const config = ConfigProvider.layer(ConfigProvider.fromEnv({ env }));
  return yield* body.pipe(Effect.provide(Layer.mergeAll(AuthLive.pipe(Layer.provide(cloud)), cloud, config)));
});

it.live("the sidebar answers a signed-in member in their active Organization", () =>
  inCloud(Effect.gen(function* () {
    const ada = yield* signUp("ada");
    expect(yield* chat(ada.caller.organization.slug, { cookie: ada.cookie }, "thread-ada")).toBe(200);
  })));

it.live("an Organization Token cannot drive the sidebar, even in its own Organization", () =>
  inCloud(Effect.gen(function* () {
    const ada = yield* signUp("ada");
    const token = yield* createOrganizationToken(ada.caller, { name: "ci", expiresInDays: 1 });
    expect(yield* chat(ada.caller.organization.slug, { authorization: `Bearer ${token.secret}` }, "thread-ci")).toBe(403);
  })));

it.live("a member must switch to an Organization before its sidebar answers them", () =>
  inCloud(Effect.gen(function* () {
    const ada = yield* signUp("ada");
    const bo = yield* signUp("bo");
    const { drizzle } = yield* Database;
    yield* drizzle.insert(member).values({ userId: ada.caller.userId, organizationId: bo.caller.organization.id });
    expect(yield* chat(bo.caller.organization.slug, { cookie: ada.cookie }, "thread-ada")).toBe(409);
  })));

it.live("a member cannot post into another member's thread, and that thread stays as it was", () =>
  inCloud(Effect.gen(function* () {
    const ada = yield* signUp("ada");
    const bo = yield* signUp("bo");
    const { drizzle } = yield* Database;
    yield* drizzle.insert(member).values({ userId: ada.caller.userId, organizationId: bo.caller.organization.id });
    expect(yield* chat(bo.caller.organization.slug, { cookie: bo.cookie }, "thread-bo")).toBe(200);
    const persistence = yield* agentPersistence({ organizationId: bo.caller.organization.id, userId: bo.caller.userId });
    const before = yield* Effect.promise(() => persistence.stores.messages.loadThread("thread-bo"));
    expect(yield* chat(ada.caller.organization.slug, { cookie: ada.cookie }, "thread-bo")).toBe(403);
    expect(yield* Effect.promise(() => persistence.stores.messages.loadThread("thread-bo"))).toEqual(before);
  })));

it.live("a Cloud without an Anthropic key answers that the agent isn't set up", () =>
  inCloud(Effect.gen(function* () {
    const ada = yield* signUp("ada");
    const answered = yield* reply(ada.caller.organization.slug, { cookie: ada.cookie }, "thread-ada");
    expect(answered.status).toBe(200);
    expect(answered.body).toContain("The Ployz agent is not set up on this Cloud yet: it needs an Anthropic API key.");
  }), {}));
