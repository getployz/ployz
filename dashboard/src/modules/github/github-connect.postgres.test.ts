import { it } from "@effect/vitest";
import { Effect, Layer } from "effect";
import { expect } from "vitest";
import { acceptGithubOffer, completeGithubConnect, listGithubOffer, startGithubConnect } from "./github-connect.server";
import { listGithubInstallationsForOrganization } from "./github.repository";
import { member, user } from "#/modules/identity/tables";
import { organization } from "#/modules/organization/tables";
import { Database } from "#/server/database.server";
import { fakeGithubApi, fakeGithubUsers } from "#/test/fake-github";
import { seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const SHOP = "00000000-0000-4000-8000-00000000c001";
const LAB = "00000000-0000-4000-8000-00000000c002";
const MANY = Array.from({ length: 120 }, (_, index) => ({ id: 1000 + index, login: `org-${index}`, type: "Organization" as const }));

const users = fakeGithubUsers({
  "ada-code": {
    login: "ada",
    installations: [{ id: 7, login: "acme", type: "Organization" }, { id: 8, login: "ada", type: "User" }],
    adminOf: ["acme"],
  },
  "mallory-code": {
    login: "mallory",
    installations: [
      { id: 7, login: "acme", type: "Organization" },
      { id: 9, login: "hopper", type: "Organization" },
      { id: 12, login: "grace", type: "User" },
    ],
    memberOf: ["hopper"],
  },
  "carol-code": {
    login: "carol",
    installations: [
      { id: 7, login: "acme", type: "Organization" },
      { id: 9, login: "hopper", type: "Organization" },
      { id: 13, login: "Carol", type: "User" },
    ],
    adminOf: ["hopper"],
  },
  "octo-code": { login: "octo", installations: MANY, adminOf: MANY.map((installation) => installation.login) },
});
const github = { ...fakeGithubApi().service, ...users.service };

const stateOf = (location: string) => new URL(location).searchParams.get("state") ?? "";
const landing = (path: string) => Object.fromEntries(new URL(path, "http://cloud").searchParams);

it.live("links only installations the person who started the connect owns on GitHub, in any Organization they manage", () =>
  Effect.gen(function* () {
    const services = yield* Layer.build(yield* storeTestCloud({ github }));
    const run = <A, E, R>(program: Effect.Effect<A, E, R>) => program.pipe(Effect.provide(services));
    const failure = <A, E extends { readonly _tag: string }, R>(program: Effect.Effect<A, E, R>) => run(Effect.flip(program)).pipe(Effect.map((error) => error._tag));
    const linked = (organizationId: string) => run(listGithubInstallationsForOrganization(organizationId)).pipe(
      Effect.map((rows) => rows.map((row) => row.installationId).sort((a, b) => a - b)),
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

    const shopInstall = stateOf(yield* run(startGithubConnect(ada, "shop", false)));
    const [body, signature] = shopInstall.split(".");
    const forged = `${Buffer.from(JSON.stringify({ organizationId: LAB, userId: ada.userId, expiresAt: Date.now() + 60_000, existing: true })).toString("base64url")}.${signature}`;
    const complete = (actor: typeof ada, params: Record<string, string>) => run(completeGithubConnect(actor, new URLSearchParams(params)));
    for (const state of ["", "nonsense", forged, `${body}.${"A".repeat(signature?.length ?? 0)}`]) {
      expect(yield* failure(completeGithubConnect(ada, new URLSearchParams({ state, code: "ada-code" })))).toBe("Forbidden");
    }
    expect(yield* failure(completeGithubConnect(bob, new URLSearchParams({ state: shopInstall, code: "ada-code" })))).toBe("Forbidden");

    expect(landing(yield* complete(ada, { state: shopInstall, code: "ada-code", installation_id: "10" }))).toEqual({ section: "github", connect: "not-yours" });
    expect(landing(yield* complete(ada, { state: shopInstall, code: "bad-code", installation_id: "8" }))).toEqual({ section: "github", connect: "failed" });
    expect(landing(yield* complete(ada, { state: shopInstall, setup_action: "request" }))).toEqual({ section: "github", connect: "pending" });
    expect(landing(yield* complete(ada, { state: shopInstall, setup_action: "request", code: "ada-code", installation_id: "8" })))
      .toEqual({ section: "github", connect: "pending" });
    // The install state names one installation; without it nothing links, even for an owner.
    expect(landing(yield* complete(ada, { state: shopInstall, code: "ada-code" }))).toEqual({ section: "github", connect: "failed" });
    expect(landing(yield* complete(ada, { state: shopInstall, code: "ada-code", installation_id: "7" }))).toEqual({ section: "github", connect: "linked" });
    expect(yield* linked(SHOP)).toEqual([7]);

    expect(landing(yield* complete(ada, { state: shopInstall, code: "ada-code", installation_id: "8" }))).toEqual({ section: "github", connect: "linked" });
    expect(yield* linked(SHOP)).toEqual([7, 8]);
    const shopExisting = stateOf(yield* run(startGithubConnect(ada, "shop", true)));
    expect(landing(yield* complete(ada, { state: shopExisting, code: "ada-code" }))).toEqual({ section: "github", connect: "none" });

    const labInstall = stateOf(yield* run(startGithubConnect(bob, "lab", false)));
    const labExisting = stateOf(yield* run(startGithubConnect(bob, "lab", true)));
    for (const installationId of ["7", "9", "12"]) {
      expect(landing(yield* complete(bob, { state: labInstall, code: "mallory-code", installation_id: installationId })))
        .toEqual({ section: "github", connect: "not-admin" });
    }
    expect(landing(yield* complete(bob, { state: labExisting, code: "mallory-code" }))).toEqual({ section: "github", connect: "not-admin" });
    expect(yield* linked(LAB)).toEqual([]);

    const offered = landing(yield* complete(bob, { state: labExisting, code: "carol-code" }));
    expect(offered["section"]).toBe("github");
    const offer = offered["offer"] ?? "";
    expect((yield* run(listGithubOffer(bob, "lab", offer))).map((account) => account.login)).toEqual(["hopper", "Carol"]);
    expect(yield* failure(listGithubOffer(ada, "lab", offer))).toBe("Forbidden");
    expect(yield* failure(listGithubOffer(bob, "shop", offer))).toBe("Forbidden");
    expect(yield* failure(acceptGithubOffer(bob, { organizationSlug: "lab", offer, installationIds: [7] }))).toBe("Validation");
    expect(yield* failure(acceptGithubOffer(bob, { organizationSlug: "lab", offer, installationIds: [9, 7] }))).toBe("Validation");
    expect(yield* linked(LAB)).toEqual([]);
    expect(yield* run(acceptGithubOffer(bob, { organizationSlug: "lab", offer, installationIds: [9] }))).toEqual({ linked: 1 });
    expect(yield* linked(LAB)).toEqual([9]);

    const adaLab = stateOf(yield* run(startGithubConnect(ada, "lab", false)));
    expect(landing(yield* complete(ada, { state: adaLab, code: "ada-code", installation_id: "7" }))).toEqual({ section: "github", connect: "linked" });
    expect(yield* linked(LAB)).toEqual([7, 9]);
    expect(yield* linked(SHOP)).toEqual([7, 8]);

    const manyLanding = yield* complete(bob, { state: labExisting, code: "octo-code" });
    expect(manyLanding.length).toBeLessThan(8192);
    expect(users.calls).toContain("/user/installations?per_page=100&page=2");
    const manyOffer = landing(manyLanding)["offer"] ?? "";
    expect((yield* run(listGithubOffer(bob, "lab", manyOffer))).map((account) => account.installationId)).toEqual(MANY.map((installation) => installation.id));
    const firstHundred = MANY.slice(0, 100).map((installation) => installation.id);
    expect(yield* run(acceptGithubOffer(bob, { organizationSlug: "lab", offer: manyOffer, installationIds: firstHundred }))).toEqual({ linked: 100 });
    expect(yield* linked(LAB)).toEqual([7, 9, ...firstHundred]);
  }), 60_000);
