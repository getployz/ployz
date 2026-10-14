import { it } from "@effect/vitest";
import { Effect, Layer } from "effect";
import { expect } from "vitest";
import { acceptGithubOffer, completeGithubConnect, listGithubOffer, startGithubConnect } from "./github-connect.server";
import { GithubObservationError, type GithubUserInstallations } from "./github-observation.api";
import { listGithubInstallationsForOrganization } from "./github.repository";
import { member, user } from "#/modules/identity/tables";
import { organization } from "#/modules/organization/tables";
import { Database } from "#/server/database.server";
import { fakeGithubApi } from "#/test/fake-github";
import { seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const SHOP = "00000000-0000-4000-8000-00000000c001";
const LAB = "00000000-0000-4000-8000-00000000c002";

const installation = (id: number, login: string, suspended = false) => ({
  id, account: { login, type: login === "acme" ? "Organization" : "User", avatar_url: null }, suspended_at: suspended ? "2026-10-01T00:00:00Z" : null,
});

const ADA: GithubUserInstallations = { login: "ada", installations: [installation(7, "acme"), installation(8, "ada")] };
const github = {
  ...fakeGithubApi().service,
  userInstallations: (code: string) => code === "ada-code"
    ? Effect.succeed(ADA)
    : Effect.fail(new GithubObservationError({ code: "request_failed", operation: "list_user_installations", retriable: false, message: "bad code" })),
};

const stateOf = (location: string) => new URL(location).searchParams.get("state") ?? "";
const landing = (path: string) => Object.fromEntries(new URL(path, "http://cloud").searchParams);

it.live("links only installations GitHub lists for the person who started the connect, in any Organization they manage", () =>
  Effect.gen(function* () {
    const services = yield* Layer.build(yield* storeTestCloud({ github }));
    const run = <A, E, R>(program: Effect.Effect<A, E, R>) => program.pipe(Effect.provide(services));
    const failure = <A, E extends { readonly _tag: string }, R>(program: Effect.Effect<A, E, R>) => run(Effect.flip(program)).pipe(Effect.map((error) => error._tag));
    const linked = (organizationId: string) => run(listGithubInstallationsForOrganization(organizationId)).pipe(
      Effect.map((rows) => rows.map((row) => row.installationId)),
    );

    const ada = { userId: yield* run(seedStoreOrganization(SHOP)) };
    const { drizzle } = yield* run(Effect.gen(function* () { return yield* Database; }));
    const [bobRow] = yield* drizzle.insert(user).values({ email: "bob@example.test", name: "Bob" }).returning();
    const bob = { userId: bobRow?.id ?? "" };
    yield* drizzle.insert(organization).values({ id: LAB, name: "Lab", slug: "lab" });
    yield* drizzle.insert(member).values([
      { userId: bob.userId, organizationId: SHOP, role: "member" },
      { userId: ada.userId, organizationId: LAB, role: "owner" },
      { userId: bob.userId, organizationId: LAB, role: "owner" },
    ]);

    expect(yield* failure(startGithubConnect(bob, "shop", false))).toBe("Forbidden");
    expect(yield* failure(startGithubConnect(bob, "shop", true))).toBe("Forbidden");

    const shopState = stateOf(yield* run(startGithubConnect(ada, "shop", false)));
    const [body, signature] = shopState.split(".");
    const forged = `${Buffer.from(JSON.stringify({ organizationId: LAB, userId: ada.userId, expiresAt: Date.now() + 60_000 })).toString("base64url")}.${signature}`;
    const complete = (actor: typeof ada, params: Record<string, string>) => run(completeGithubConnect(actor, new URLSearchParams(params)));
    for (const state of ["", "nonsense", forged, `${body}.${"A".repeat(signature?.length ?? 0)}`]) {
      expect(yield* failure(completeGithubConnect(ada, new URLSearchParams({ state, code: "ada-code" })))).toBe("Forbidden");
    }
    expect(yield* failure(completeGithubConnect(bob, new URLSearchParams({ state: shopState, code: "ada-code" })))).toBe("Forbidden");

    expect(landing(yield* complete(ada, { state: shopState, code: "ada-code", installation_id: "10" }))).toEqual({ section: "github", connect: "not-yours" });
    expect(yield* linked(SHOP)).toEqual([7]);
    expect(landing(yield* complete(ada, { state: shopState, code: "bad-code", installation_id: "8" }))).toEqual({ section: "github", connect: "failed" });
    expect(landing(yield* complete(ada, { state: shopState, setup_action: "request" }))).toEqual({ section: "github", connect: "pending" });
    expect(yield* linked(SHOP)).toEqual([7]);

    expect(landing(yield* complete(ada, { state: shopState, code: "ada-code", installation_id: "8" }))).toEqual({ section: "github", connect: "linked" });
    expect(yield* linked(SHOP)).toEqual([7, 8]);
    expect(landing(yield* complete(ada, { state: shopState, code: "ada-code" }))).toEqual({ section: "github", connect: "none" });

    const labState = stateOf(yield* run(startGithubConnect(ada, "lab", true)));
    const offered = landing(yield* complete(ada, { state: labState, code: "ada-code" }));
    expect(offered["section"]).toBe("github");
    const offer = offered["offer"] ?? "";
    expect((yield* run(listGithubOffer(ada, "lab", offer))).map((account) => account.login)).toEqual(["acme", "ada"]);
    expect(yield* failure(listGithubOffer(bob, "lab", offer))).toBe("Forbidden");
    expect(yield* failure(listGithubOffer(ada, "shop", offer))).toBe("Forbidden");
    expect(yield* failure(acceptGithubOffer(ada, { organizationSlug: "lab", offer, installationIds: [10] }))).toBe("Validation");
    expect(yield* linked(LAB)).toEqual([]);

    expect(yield* run(acceptGithubOffer(ada, { organizationSlug: "lab", offer, installationIds: [7] }))).toEqual({ linked: 1 });
    expect(yield* linked(LAB)).toEqual([7]);
    expect(yield* linked(SHOP)).toEqual([7, 8]);
  }), 60_000);
