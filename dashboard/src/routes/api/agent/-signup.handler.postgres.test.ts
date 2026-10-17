import { it } from "@effect/vitest";
import { eq } from "drizzle-orm";
import { Effect, Layer } from "effect";
import { expect } from "vitest";
import { resolveCaller } from "#/modules/identity/caller.server";
import { member, organizationToken, user } from "#/modules/identity/tables";
import { organization } from "#/modules/organization/tables";
import { handleAgentSignup, signupMethodNotAllowed } from "#/routes/api/agent/-signup.handler";
import { AuthLive } from "#/server/auth.server";
import { Database } from "#/server/database.server";
import { storeTestCloud } from "#/test/store-cloud";

const DAY_MS = 24 * 60 * 60 * 1000;

interface SignupBody {
  readonly token: string;
  readonly organization: { readonly id: string; readonly slug: string };
  readonly claimUrl: string | null;
  readonly expiresAt: string;
}

const inCloud = <A, E, R>(body: Effect.Effect<A, E, R>) => Effect.gen(function* () {
  const cloud = yield* storeTestCloud();
  return yield* body.pipe(Effect.provide(Layer.mergeAll(AuthLive.pipe(Layer.provide(cloud)), cloud)));
});

const signUp = Effect.fn(function* (ip: string) {
  const response = yield* handleAgentSignup(ip);
  const body = yield* Effect.promise(() => response.json() as Promise<SignupBody>);
  return { status: response.status, headers: response.headers, body };
});

const bearer = (token: string) => new Headers({ authorization: `Bearer ${token}` });

it.live("signs up an agent with a token that acts as the owner of a fresh sandbox for 24 hours", () =>
  inCloud(Effect.gen(function* () {
    const before = Date.now();
    const { status, headers, body } = yield* signUp("203.0.113.1");
    expect(status).toBe(201);
    expect(headers.get("cache-control")).toBe("no-store");
    expect(body.claimUrl).toBeNull();
    const sandbox = body.organization;
    expect(sandbox.slug).toMatch(/^anon-[a-z2-7]{8}$/);
    const expiresAt = Date.parse(body.expiresAt);
    expect(expiresAt).toBeGreaterThanOrEqual(before + DAY_MS);
    expect(expiresAt).toBeLessThanOrEqual(Date.now() + DAY_MS);

    const caller = yield* resolveCaller(bearer(body.token));
    expect(caller.organization).toEqual(sandbox);
    expect(caller.role).toBe("owner");

    const { drizzle } = yield* Database;
    const [agent] = yield* drizzle.select().from(user).where(eq(user.id, caller.userId));
    expect(agent?.isAnonymous).toBe(true);
    const memberships = yield* drizzle
      .select({ slug: organization.slug, name: organization.name })
      .from(member)
      .innerJoin(organization, eq(member.organizationId, organization.id))
      .where(eq(member.userId, caller.userId));
    expect(memberships).toEqual([{ slug: sandbox.slug, name: "Agent sandbox" }]);
  })));

it.live("refuses the token once it expires", () =>
  inCloud(Effect.gen(function* () {
    const { body } = yield* signUp("203.0.113.2");
    const { drizzle } = yield* Database;
    yield* drizzle.update(organizationToken).set({ expiresAt: new Date(Date.now() - 1000) });
    const refused = yield* resolveCaller(bearer(body.token)).pipe(Effect.flip);
    expect(refused._tag).toBe("Unauthorized");
  })));

it.live("admits five signups an hour from one address, then refuses with Retry-After and makes nothing", () =>
  inCloud(Effect.gen(function* () {
    for (let i = 0; i < 5; i++) expect((yield* signUp("203.0.113.3")).status).toBe(201);
    const refused = yield* signUp("203.0.113.3");
    expect(refused.status).toBe(429);
    expect(Number(refused.headers.get("retry-after"))).toBeGreaterThan(3500);
    expect(refused.body).toMatchObject({ _tag: "PublicError", code: "VALIDATION_FAILED" });

    const { drizzle } = yield* Database;
    expect(yield* drizzle.select().from(user).where(eq(user.isAnonymous, true))).toHaveLength(5);
    expect((yield* signUp("203.0.113.4")).status).toBe(201);
  })));

it.live("answers GET with 405 in the public error shape", () =>
  Effect.gen(function* () {
    const response = signupMethodNotAllowed();
    expect(response.status).toBe(405);
    expect(response.headers.get("allow")).toBe("POST");
    const body = yield* Effect.promise(() => response.json() as Promise<unknown>);
    expect(body).toMatchObject({ _tag: "PublicError", code: "VALIDATION_FAILED", message: "Sign up with POST." });
  }));
