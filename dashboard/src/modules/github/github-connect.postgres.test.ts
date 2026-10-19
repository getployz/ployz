import { it } from "@effect/vitest";
import { Deferred, Effect, Fiber, Layer, Schedule } from "effect";
import { eq, sql } from "drizzle-orm";
import { Inngest } from "inngest";
import { expect, vi } from "vitest";
import { MAX_OFFERED_INSTALLATIONS } from "./github-connect";
import { acceptGithubOfferInput } from "./github-connect.functions";
import { acceptGithubOffer, completeGithubConnect, listGithubOffer, listOrganizationGithub, startGithubConnect } from "./github-connect.server";
import {
  deleteCachedGithubRepositoriesById, findGithubRepositoryByNameForOrganization, listGithubInstallationsForOrganization, lockGithubRepositoryCache,
  pruneCachedGithubRepositories, upsertCachedGithubRepositories, upsertGithubInstallation,
} from "./github.repository";
import { processGithubInstallationRepositoriesEvent } from "./github.server";
import { githubRepositoryCache } from "./tables";
import { GITHUB_REPOSITORIES_PAGE_SIZE, type GithubApi, type GithubApiService } from "./github-observation.api";
import { executeSyncGithubRepositories } from "./inngest-sync/sync";
import { member, user } from "#/modules/identity/tables";
import { organization } from "#/modules/organization/tables";
import type { AppConfig } from "#/server/config.server";
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
// Repositories GitHub no longer grants the app.
const revoked = new Set<number>();
// Repositories renamed on GitHub, which made them private on a new default branch too.
const renamed = new Map<number, string>();
// Runs once, after a writer holding the cache lock re-reads installation 9's second page and before it gets it.
let afterSecondPage: Effect.Effect<void> | undefined;
// Runs once, while a writer first lists installation 9, before it holds any connection.
let whileListing: Effect.Effect<void> | undefined;
const installationRepositories: GithubApiService["installationRepositories"] = ({ installationId, page, etag }) =>
  listRepositories({ installationId, page, etag }).pipe(Effect.tap(() => {
    if (installationId !== 9) return Effect.void;
    if (etag === undefined && page === 1) {
      const hook = whileListing;
      whileListing = undefined;
      return hook ?? Effect.void;
    }
    if (etag !== undefined && page === 2) {
      const hook = afterSecondPage;
      afterSecondPage = undefined;
      return hook ?? Effect.void;
    }
    return Effect.void;
  }));
// Answers a conditional read with "not-modified" while the page's ids, names and total still match its ETag.
const listRepositories: GithubApiService["installationRepositories"] = ({ installationId, page, etag }) => {
  const granted = Array.from({ length: REPOSITORY_COUNTS.get(installationId) ?? 0 }, (_, index) => installationId * 1000 + index)
    .filter((id) => !revoked.has(id));
  const repositories = granted.slice((page - 1) * GITHUB_REPOSITORIES_PAGE_SIZE, page * GITHUB_REPOSITORIES_PAGE_SIZE).map(repository);
  const current = JSON.stringify([granted.length, repositories.map((listed) => [listed.id, listed.name])]);
  if (etag === current) return Effect.succeed("not-modified" as const);
  return Effect.succeed({ total_count: granted.length, repositories, etag: current });
};
const repository = (id: number) => {
  const name = renamed.get(id) ?? `repo-${id}`;
  return {
    id, name, full_name: `owner/${name}`, private: renamed.has(id), default_branch: renamed.has(id) ? "trunk" : "main",
    html_url: `https://github.test/owner/${name}`, updated_at: new Date("2026-01-01T00:00:00Z"),
  };
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

// A cloud where Ada owns `shop` and may connect `hopper`, installation 9, which GitHub already told it about.
const hopperCloud = Effect.fn(function* () {
  const inngest = new Inngest({ id: "github-connect-test" });
  const send = vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });
  revoked.clear();
  renamed.clear();
  afterSecondPage = undefined;
  whileListing = undefined;
  const services = yield* Layer.build(yield* storeTestCloud({ github, inngest }));
  const run = <A, E, R>(program: Effect.Effect<A, E, R>) => program.pipe(Effect.provide(services));
  const ada = { userId: yield* run(seedStoreOrganization(SHOP)) };
  yield* run(upsertGithubInstallation({ installationId: 9, accountLogin: "hopper", accountType: "Organization", accountAvatarUrl: null }));
  const connectHopper = Effect.gen(function* () {
    const existing = stateOf(yield* run(startGithubConnect(ada, "shop", true)));
    const offer = landing(yield* run(completeGithubConnect(ada, new URLSearchParams({ state: existing, code: "carol-code" }))))["offer"] ?? "";
    return yield* run(acceptGithubOffer(ada, { organizationSlug: "shop", offer, installationIds: [9] }));
  });
  const sync = Effect.promise(() => executeSyncGithubRepositories(
    { event: { data: { installationId: 9 } }, step: { run: async (_id, operation, ...input) => JSON.parse(JSON.stringify(await operation(...input) ?? null)) } },
    (program) => Effect.runPromise(program.pipe(Effect.provide(services))),
  ));
  const cached = run(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const rows = yield* drizzle.select({ repositoryId: githubRepositoryCache.repositoryId }).from(githubRepositoryCache)
      .where(eq(githubRepositoryCache.installationId, 9));
    return new Set(rows.map((row) => row.repositoryId));
  }));
  // Starts `program` as another request would: on its own root fiber, outside any transaction the caller is in.
  const elsewhere = <A, E>(program: Effect.Effect<A, E, Database | GithubApi | AppConfig>) =>
    Effect.sync(() => Effect.runFork(program.pipe(Effect.provide(services))));
  // Returns once `fiber` has finished or is waiting on installation 9's repository cache lock.
  const settled = <A, E>(fiber: Fiber.Fiber<A, E>) => Effect.raceFirst(
    Fiber.await(fiber).pipe(Effect.asVoid),
    Effect.promise(() => Effect.runPromise(run(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      const [waiting] = yield* drizzle.select({ count: sql<number>`count(*)::int` }).from(sql`pg_locks`)
        .where(sql`locktype = 'advisory' and not granted and objid = 9 and objsubid = 2`);
      if ((waiting?.count ?? 0) === 0) return yield* Effect.fail("not waiting");
    }).pipe(Effect.retry({ schedule: Schedule.spaced("10 millis") }), Effect.orDie)))),
  );
  return { send, run, connectHopper, sync, cached, elsewhere, settled };
});

it.live("keeps a repository the connect listed when a sync that listed before GitHub granted it finishes later", () =>
  Effect.gen(function* () {
    const { connectHopper, sync, cached, elsewhere, settled } = yield* hopperCloud();
    yield* sync;

    // A sync lists while GitHub has 9120 revoked. GitHub grants it again, and Ada connects, before that sync prunes.
    revoked.add(9120);
    let connect: Fiber.Fiber<unknown, unknown> | undefined;
    afterSecondPage = Effect.gen(function* () {
      revoked.delete(9120);
      connect = yield* elsewhere(connectHopper);
      yield* settled(connect);
    });
    yield* sync;
    if (connect === undefined) throw new Error("The sync never listed its second page.");
    yield* Fiber.join(connect);
    expect((yield* cached).has(9120)).toBe(true);
  }), 60_000);

it.live("removes a repository GitHub revoked while the connect listed it, once the connect has written", () =>
  Effect.gen(function* () {
    const { connectHopper, cached, elsewhere, settled } = yield* hopperCloud();

    let webhook: Fiber.Fiber<unknown, unknown> | undefined;
    afterSecondPage = Effect.gen(function* () {
      revoked.add(9120);
      webhook = yield* elsewhere(processGithubInstallationRepositoriesEvent({
        deliveryId: "delivery-removed-9120", action: "removed", installation: { id: 9 },
        repositories_added: [], repositories_removed: [{ ...repository(9120), updated_at: "2026-01-01T00:00:00Z" }], sender: { id: 1, login: "carol" },
      }));
      yield* settled(webhook);
    });
    yield* connectHopper;
    if (webhook === undefined) throw new Error("The connect never listed its second page.");
    yield* Fiber.join(webhook);
    const repositories = yield* cached;
    expect(repositories.has(9120)).toBe(false);
    expect(repositories.size).toBe(149);
  }), 60_000);

it.live("writes what GitHub lists and drops the rest, whatever stamps another writer's clock gave the rows", () =>
  Effect.gen(function* () {
    const { run, connectHopper, sync, cached } = yield* hopperCloud();
    yield* connectHopper;
    // A writer whose clock runs an hour ahead cached 9000 under its old name, and 9999, which GitHub never granted.
    yield* run(upsertCachedGithubRepositories({
      installationId: 9,
      repositories: [9000, 9999].map((id) => ({ ...repository(id), repo_updated_at: "2026-01-01T00:00:00Z" })),
      syncedAt: new Date(Date.now() + 3_600_000),
    }));
    renamed.set(9000, "secret");

    expect(yield* sync).toEqual({ installationId: 9, processedPageCount: 2, processedRepoCount: 150, deletedRepositoryCount: 1 });
    expect(yield* run(findGithubRepositoryByNameForOrganization({ organizationId: SHOP, fullName: "owner/secret" })))
      .toEqual({ fullName: "owner/secret", repositoryId: 9000, installationId: 9, defaultBranch: "trunk" });
    expect((yield* cached).has(9999)).toBe(false);
  }), 60_000);

it.live("leaves the listing to a queued sync when another writer holds the cache past the connect's wait", () =>
  Effect.gen(function* () {
    const { send, run, connectHopper, cached } = yield* hopperCloud();
    const release = yield* Deferred.make<void>();
    const held = yield* Deferred.make<void>();
    const holder = yield* Effect.forkDetach(run(Effect.gen(function* () {
      const database = yield* Database;
      yield* database.transaction(Effect.gen(function* () {
        yield* lockGithubRepositoryCache(9);
        yield* Deferred.succeed(held, undefined);
        yield* Deferred.await(release);
      }));
    })));
    yield* Deferred.await(held);

    expect(yield* connectHopper).toEqual({ linked: 1 });
    expect((yield* cached).size).toBe(0);
    expect(send.mock.calls.flatMap(([events]) => [events].flat()).map((event) => event.data))
      .toEqual([{ installationId: 9, reason: "manual-refresh" }]);
    yield* Deferred.succeed(release, undefined);
    yield* Fiber.join(holder);
  }), 60_000);

it.live("returns within its wait and leaves the listing to a queued sync while other writers hold every pooled connection", () =>
  Effect.gen(function* () {
    const { send, connectHopper, cached, elsewhere } = yield* hopperCloud();
    const release = yield* Deferred.make<void>();
    const holders: Fiber.Fiber<unknown, unknown>[] = [];
    // While the connect lists GitHub, other writers take every connection in node-postgres's default pool of 10.
    whileListing = Effect.gen(function* () {
      const held = yield* Effect.forEach(Array.from({ length: 10 }), () => Deferred.make<void>());
      for (const taken of held) {
        holders.push(yield* elsewhere(Effect.gen(function* () {
          const database = yield* Database;
          yield* database.transaction(Effect.gen(function* () {
            yield* Deferred.succeed(taken, undefined);
            yield* Deferred.await(release);
          }));
        })));
      }
      yield* Effect.forEach(held, Deferred.await, { discard: true });
    });

    const started = Date.now();
    expect(yield* connectHopper).toEqual({ linked: 1 });
    expect(Date.now() - started).toBeLessThan(7_000);
    expect(send.mock.calls.flatMap(([events]) => [events].flat()).map((event) => event.data))
      .toEqual([{ installationId: 9, reason: "manual-refresh" }]);

    yield* Deferred.succeed(release, undefined);
    for (const holder of holders) yield* Fiber.join(holder);
    // The connect's sync takes a connection once one frees, so none leaked, and caches the listing.
    yield* cached.pipe(
      Effect.flatMap((repositories) => repositories.size === 150 ? Effect.void : Effect.fail("not cached yet")),
      Effect.retry({ schedule: Schedule.spaced("20 millis") }),
      Effect.timeout("10 seconds"),
    );
  }), 60_000);

it.live("caches and prunes in one call past the 65,535 parameters Postgres binds to a statement", () =>
  Effect.gen(function* () {
    const { run, cached } = yield* hopperCloud();
    // 8,000 rows of 9 columns would bind 72,000 parameters, and 70,000 listed ids would bind one each.
    yield* run(upsertCachedGithubRepositories({
      installationId: 9,
      repositories: Array.from({ length: 8000 }, (_, index) => ({ ...repository(9000 + index), repo_updated_at: "2026-01-01T00:00:00Z" })),
      syncedAt: new Date(),
    }));
    expect((yield* cached).size).toBe(8000);
    const listed = Array.from({ length: 70_000 }, (_, index) => 9000 + index).filter((id) => id !== 9001);
    expect(yield* run(pruneCachedGithubRepositories(9, listed))).toBe(1);
    yield* run(deleteCachedGithubRepositoriesById(9, listed));
    expect((yield* cached).size).toBe(0);
  }), 60_000);
