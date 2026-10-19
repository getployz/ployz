import { it } from "@effect/vitest";
import { Effect, Layer } from "effect";
import { Inngest } from "inngest";
import { expect, vi } from "vitest";
import { MAX_OFFERED_INSTALLATIONS } from "./github-connect";
import { acceptGithubOfferInput } from "./github-connect.functions";
import { acceptGithubOffer, completeGithubConnect, listGithubOffer, listOrganizationGithub, startGithubConnect } from "./github-connect.server";
import { listInstallationReposPage } from "./github.api";
import { findGithubRepositoryByNameForOrganization, listGithubInstallationsForOrganization, pruneCachedGithubRepositories, upsertCachedGithubRepositories } from "./github.repository";
import { GITHUB_REPOSITORIES_PAGE_SIZE, type GithubApiService } from "./github-observation.api";
import { executeSyncGithubRepositories } from "./inngest-sync/sync";
import type { PloyzStepTools } from "#/modules/inngest/client";
import { member, user } from "#/modules/identity/tables";
import { organization } from "#/modules/organization/tables";
import { Database } from "#/server/database.server";
import { fakeGithubApi, fakeGithubUsers } from "#/test/fake-github";
import { seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const SHOP = "00000000-0000-4000-8000-00000000c001";
const LAB = "00000000-0000-4000-8000-00000000c002";
// Past the offer bound, with ids as long as the installation column stores, so the offer is as large as it gets.
const MANY = Array.from({ length: MAX_OFFERED_INSTALLATIONS + 20 }, (_, index) => ({ id: 2_147_483_647 - index, login: `org-${index}`, type: "Organization" as const }));
const OFFERED = MANY.slice(0, MAX_OFFERED_INSTALLATIONS).map((installation) => installation.id);

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
// Installation 9 holds more repositories than one page lists.
const REPOSITORY_COUNTS = new Map([[8, 3], [9, 150]]);
// Repositories renamed on GitHub, which made them private on a new default branch too.
const renamed = new Map<number, string>();
// Runs once, just before installation 9's second page is listed, to land another sync's write mid-listing.
let beforeSecondPage: Effect.Effect<void> | undefined;
const installationRepositories: GithubApiService["installationRepositories"] = ({ installationId, page }) => Effect.suspend(() => {
  const interleaved = installationId === 9 && page === 2 ? beforeSecondPage : undefined;
  if (interleaved !== undefined) beforeSecondPage = undefined;
  return interleaved ?? Effect.void;
}).pipe(Effect.andThen(() => listRepositories({ installationId, page })));
const listRepositories: GithubApiService["installationRepositories"] = ({ installationId, page }) => {
  const total = REPOSITORY_COUNTS.get(installationId) ?? 0;
  const ids = Array.from({ length: total }, (_, index) => installationId * 1000 + index)
    .slice((page - 1) * GITHUB_REPOSITORIES_PAGE_SIZE, page * GITHUB_REPOSITORIES_PAGE_SIZE);
  return Effect.succeed({
    total_count: total,
    repositories: ids.map((id) => {
      const name = renamed.get(id) ?? `repo-${id}`;
      return {
        id, name, full_name: `owner/${name}`, private: renamed.has(id), default_branch: renamed.has(id) ? "trunk" : "main",
        html_url: `https://github.test/owner/${name}`, updated_at: new Date("2026-01-01T00:00:00Z"),
      };
    }),
  });
};
const github = { ...fakeGithubApi().service, ...users.service, installationRepositories };

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
    expect(users.calls).toContain("/user/installations?per_page=100&page=4");
    expect((yield* run(listGithubOffer(bob, "lab", manyOffer))).map((account) => account.installationId)).toEqual(OFFERED);
    const everyOffered = yield* Effect.promise(async () =>
      acceptGithubOfferInput["~standard"].validate({ organizationSlug: "lab", offer: manyOffer, installationIds: OFFERED }));
    if (everyOffered.issues !== undefined) throw new Error(`accept rejected the full offer: ${everyOffered.issues[0]?.message}`);
    expect(yield* run(acceptGithubOffer(bob, everyOffered.value))).toEqual({ linked: MAX_OFFERED_INSTALLATIONS });
    expect(yield* linked(LAB)).toEqual([7, 9, ...[...OFFERED].sort((a, b) => a - b)]);
  }), 60_000);

it.live("lists a connected account's repositories as soon as the connect returns", () =>
  Effect.gen(function* () {
    const services = yield* Layer.build(yield* storeTestCloud({ github }));
    const run = <A, E, R>(program: Effect.Effect<A, E, R>) => program.pipe(Effect.provide(services));
    const ada = { userId: yield* run(seedStoreOrganization(SHOP)) };
    const counts = () => run(listOrganizationGithub(ada, "shop")).pipe(
      Effect.map(({ accounts }) => Object.fromEntries(accounts.map((account) => [account.login, account.repositories]))),
    );

    const install = stateOf(yield* run(startGithubConnect(ada, "shop", false)));
    yield* run(completeGithubConnect(ada, new URLSearchParams({ state: install, code: "ada-code", installation_id: "8" })));
    expect(yield* counts()).toEqual({ acme: 0, ada: 3 });

    const existing = stateOf(yield* run(startGithubConnect(ada, "shop", true)));
    const offer = landing(yield* run(completeGithubConnect(ada, new URLSearchParams({ state: existing, code: "carol-code" }))))["offer"] ?? "";
    yield* run(acceptGithubOffer(ada, { organizationSlug: "shop", offer, installationIds: [9] }));
    expect(yield* counts()).toEqual({ acme: 0, ada: 3, hopper: 150 });
  }), 60_000);

it.live("keeps every listed repository when an older sync writes while the connect lists", () =>
  Effect.gen(function* () {
    const inngest = new Inngest({ id: "github-connect-test" });
    const send = vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });
    const services = yield* Layer.build(yield* storeTestCloud({ github, inngest }));
    const run = <A, E, R>(program: Effect.Effect<A, E, R>) => program.pipe(Effect.provide(services));
    const ada = { userId: yield* run(seedStoreOrganization(SHOP)) };
    const hopper = () => run(listOrganizationGithub(ada, "shop")).pipe(
      Effect.map(({ accounts }) => accounts.find((account) => account.login === "hopper")?.repositories),
    );

    // A sync queued earlier, say by GitHub's installation webhook, started a second ago and writes its first page late.
    const olderSyncStartedAt = new Date(Date.now() - 1000);
    beforeSecondPage = run(Effect.gen(function* () {
      const page = yield* listInstallationReposPage(9, 1);
      yield* upsertCachedGithubRepositories({ installationId: 9, repositories: page.repositories, syncedAt: olderSyncStartedAt });
    })).pipe(Effect.orDie);
    const existing = stateOf(yield* run(startGithubConnect(ada, "shop", true)));
    const offer = landing(yield* run(completeGithubConnect(ada, new URLSearchParams({ state: existing, code: "carol-code" }))))["offer"] ?? "";
    yield* run(acceptGithubOffer(ada, { organizationSlug: "shop", offer, installationIds: [9] }));
    expect(beforeSecondPage).toBeUndefined();
    expect(yield* hopper()).toBe(150);

    yield* run(pruneCachedGithubRepositories(9, olderSyncStartedAt));
    const queued = send.mock.calls.flatMap(([events]) => [events].flat());
    expect(queued.map((event) => event.data)).toEqual([{ installationId: 9, reason: "manual-refresh" }]);
    const step: PloyzStepTools["run"] = async (_id, operation, ...input) => JSON.parse(JSON.stringify(await operation(...input) ?? null));
    for (const event of queued) {
      yield* Effect.promise(() => executeSyncGithubRepositories(
        { event: { data: event.data }, step: { run: step } },
        (program) => Effect.runPromise(program.pipe(Effect.provide(services))),
      ));
    }
    expect(yield* hopper()).toBe(150);
  }), 60_000);

it.live("keeps a repository GitHub added after a sync that started during the connect listed", () =>
  Effect.gen(function* () {
    const services = yield* Layer.build(yield* storeTestCloud({ github }));
    const run = <A, E, R>(program: Effect.Effect<A, E, R>) => program.pipe(Effect.provide(services));
    const ada = { userId: yield* run(seedStoreOrganization(SHOP)) };
    const hopper = () => run(listOrganizationGithub(ada, "shop")).pipe(
      Effect.map(({ accounts }) => accounts.find((account) => account.login === "hopper")?.repositories),
    );

    // A sync starts after the connect does and lists only the first page; GitHub then grants the rest, which the connect lists.
    let syncStartedAt = new Date(0);
    beforeSecondPage = Effect.gen(function* () {
      yield* Effect.sleep("5 millis");
      syncStartedAt = new Date();
      const page = yield* listInstallationReposPage(9, 1);
      yield* upsertCachedGithubRepositories({ installationId: 9, repositories: page.repositories, syncedAt: syncStartedAt });
    }).pipe(run, Effect.orDie);
    const existing = stateOf(yield* run(startGithubConnect(ada, "shop", true)));
    const offer = landing(yield* run(completeGithubConnect(ada, new URLSearchParams({ state: existing, code: "carol-code" }))))["offer"] ?? "";
    yield* run(acceptGithubOffer(ada, { organizationSlug: "shop", offer, installationIds: [9] }));
    expect(beforeSecondPage).toBeUndefined();

    yield* run(pruneCachedGithubRepositories(9, syncStartedAt));
    expect(yield* hopper()).toBe(150);
  }), 60_000);

it.live("keeps what the connect read when a sync that listed earlier writes after it", () =>
  Effect.gen(function* () {
    const services = yield* Layer.build(yield* storeTestCloud({ github }));
    const run = <A, E, R>(program: Effect.Effect<A, E, R>) => program.pipe(Effect.provide(services));
    const ada = { userId: yield* run(seedStoreOrganization(SHOP)) };
    const found = (fullName: string) => run(findGithubRepositoryByNameForOrganization({ organizationId: SHOP, fullName }));

    const olderSyncStartedAt = new Date(Date.now() - 1000);
    const olderPage = yield* run(listInstallationReposPage(9, 1));
    renamed.set(9000, "secret");
    const existing = stateOf(yield* run(startGithubConnect(ada, "shop", true)));
    const offer = landing(yield* run(completeGithubConnect(ada, new URLSearchParams({ state: existing, code: "carol-code" }))))["offer"] ?? "";
    yield* run(acceptGithubOffer(ada, { organizationSlug: "shop", offer, installationIds: [9] }));
    yield* run(upsertCachedGithubRepositories({ installationId: 9, repositories: olderPage.repositories, syncedAt: olderSyncStartedAt }));
    yield* run(pruneCachedGithubRepositories(9, olderSyncStartedAt));
    renamed.clear();

    expect(yield* found("owner/secret")).toEqual({ fullName: "owner/secret", repositoryId: 9000, installationId: 9, defaultBranch: "trunk" });
    expect(yield* found("owner/repo-9000")).toBeNull();
  }), 60_000);
