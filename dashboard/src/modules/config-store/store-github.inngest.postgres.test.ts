import { it } from "@effect/vitest";
import { InngestTestEngine } from "@inngest/test";
import type { ConfigCommand, JsonValue, ConfigTrusted } from "@ployz/sdk";
import { eq } from "drizzle-orm";
import { Effect } from "effect";
import { Inngest } from "inngest";
import { expect } from "vitest";
import { storeSystem } from "#/modules/config-store/config-store.server";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import { pinStoreSources } from "#/modules/config-store/store-deployment.server";
import { createStoreGithubCheckSuite, createStoreGithubPush } from "#/modules/config-store/store-github.inngest";
import { githubRepositoryCache } from "#/modules/github/tables";
import { account } from "#/modules/identity/tables";
import { githubCheckSuiteReceivedEvent, githubPushReceivedEvent } from "#/modules/inngest/events";
import { Database } from "#/server/database.server";
import { makeInngestEffectRunner } from "#/server/run.server";
import { fakeGithubApiBy } from "#/test/fake-github";
import { enrollStoreServer, seedStoreOrganization, seedStoreGitService, storeTestCloud } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000b001";
const PROJECT = "00000000-0000-4000-8000-00000000b002";
const ENVIRONMENT = "00000000-0000-4000-8000-00000000b003";
const SERVICE = "00000000-0000-4000-8000-00000000b004";
const H1 = "1".repeat(40);
const H2 = "2".repeat(40);
const here = { project: null, environment: null };

/** GitHub with `acme/web` (42) through installation 7: `main` at `state.head`, suite 9 as `state.suite` says. */
function github(state: { head: string; suite: { status: string; conclusion: string | null; updated_at: string } }) {
  return fakeGithubApiBy(({ path }): JsonValue => path.includes("/git/ref/")
    ? { ref: "refs/heads/main", object: { type: "commit", sha: state.head } }
    : path.includes("/compare/")
      ? { status: "ahead", files: [{ filename: "src/app.ts" }] }
      : path.includes("/check-suites/")
        ? { id: 9, head_sha: H2, ...state.suite }
        : { id: 42, full_name: "acme/web", private: true });
}

const push = (after: string) => ({
  name: githubPushReceivedEvent,
  data: {
    kind: "push", installationId: 7, repositoryId: 42, ref: "refs/heads/main", branch: "main", beforeSha: H1,
    afterSha: after, created: false, deleted: false, forced: false, deliveryId: `push-${after}`, branchKey: "7:42:refs/heads/main",
  },
});

const suite = (updated: number) => ({
  name: githubCheckSuiteReceivedEvent,
  data: {
    kind: "check_suite", action: "completed", installationId: 7, repositoryId: 42, checkSuiteId: 9, headSha: H2,
    status: "completed", conclusion: "success", sourceUpdatedAt: new Date(updated).toISOString(),
    deliveryId: `suite-${updated}`, checkSuiteKey: "7:42:9",
  },
});

it.live(
  "pushes and CI reach the Store: Saved State auto-deploys pinned to the head, wait-for-CI holds it, replays add nothing",
  () =>
    Effect.gen(function* () {
      const state = { head: H1, suite: { status: "in_progress", conclusion: null as string | null, updated_at: "2026-09-29T10:00:00Z" } };
      const githubApi = github(state);
      const services = yield* storeTestCloud({ github: githubApi.service });
      // A member of the Organization installed the GitHub App as installation 7.
      yield* Effect.all([seedStoreOrganization(ORGANIZATION), enrollStoreServer(ORGANIZATION)]).pipe(Effect.provide(services));
      const store = yield* cloudStore.pipe(Effect.provide(services));
      const write = (command: ConfigCommand, trusted?: ConfigTrusted) =>
        Effect.promise(() => store.write(ORGANIZATION, command, trusted));
      yield* seedStoreGitService(ORGANIZATION, { project: PROJECT, environment: ENVIRONMENT, service: SERVICE }).pipe(Effect.provide(services));
      yield* write({ command: "publish", environment: here, version: null, accept_volume_loss: [] });

      const runner: Parameters<typeof createStoreGithubPush>[1] = makeInngestEffectRunner((program) => Effect.runPromise(program.pipe(
        Effect.provide(services))));
      const inngest = new Inngest({ id: "store-github-test" });
      const sent: string[] = [];
      const run = (event: ReturnType<typeof push> | ReturnType<typeof suite>) => Effect.promise(async () =>
        (await new InngestTestEngine({
          function: event.name === githubPushReceivedEvent ? createStoreGithubPush(inngest, runner) : createStoreGithubCheckSuite(inngest, runner),
          events: [event],
          steps: [{ id: "dispatch", handler: () => sent.push(event.name) }],
        }).execute()).result as { admitted: string[] });
      const view = (deployment: string | undefined) =>
        Effect.promise(() => store.read(ORGANIZATION, { query: "deployment", id: deployment ?? "" }));

      // The first push deploys Saved State, pinned to the head GitHub has now.
      const first = yield* run(push(H1));
      expect(first.admitted).toHaveLength(1);
      expect(yield* view(first.admitted[0])).toMatchObject({ builds: [{ commit: H1 }] });
      // Cloud reads the push as itself, once for all linked Organizations.
      expect(githubApi.calls.filter((call) => !call.path.includes("/compare/")).map((call) => call.access))
        .toEqual([{ type: "installation", installationId: 7 }, { type: "installation", installationId: 7 }]);
      // A redelivery sees the same head: nothing more.
      expect((yield* run(push(H1))).admitted).toEqual([]);

      // With wait-for-CI, the next push waits for its check suite.
      yield* write({ command: "edit", environment: here, expect: null, changes: [{ op: "set", path: "web.waitForCi", value: true }] });
      state.head = H2;
      expect((yield* run(push(H2))).admitted).toEqual([]);
      expect((yield* run(suite(1))).admitted).toEqual([]);
      state.suite = { status: "completed", conclusion: "success", updated_at: "2026-09-29T10:05:00Z" };
      const passed = yield* run(suite(2));
      expect(passed.admitted).toHaveLength(1);
      expect(yield* view(passed.admitted[0])).toMatchObject({ builds: [{ commit: H2 }] });
      expect((yield* run(suite(2))).admitted).toEqual([]);
      // Only the two admissions were dispatched.
      expect(sent).toEqual([githubPushReceivedEvent, githubCheckSuiteReceivedEvent]);
    }),
  60_000,
);

const MERGE = "3".repeat(40);
const PAST = "4".repeat(40);

it.live(
  "a redelivered push past a merge whose closed delivery never came deploys what the merge landed",
  () =>
    Effect.gen(function* () {
      const state = { head: H1, merged: false };
      const services = yield* storeTestCloud({ github: fakeGithubApiBy(({ path }): JsonValue =>
        path.includes("/git/ref/")
          ? { ref: "refs/heads/main", object: { type: "commit", sha: state.head } }
          : path.includes("/compare/")
            ? { status: "ahead", files: [{ filename: "src/app.ts" }] }
            : path.endsWith("/pulls/5")
              ? {
                number: 5, title: "Add search", user: { id: 7, login: "ada", type: "User" }, head: { ref: "search", sha: H2, repo: { id: 42 } },
                base: { ref: "main" }, merged: state.merged, merge_commit_sha: state.merged ? MERGE : null, commits: 1,
                state: state.merged ? "closed" : "open", updated_at: state.merged ? "2026-09-29T11:00:00Z" : "2026-09-29T10:00:00Z",
              }
              : { id: 42, full_name: "acme/web", private: true }).service });
      yield* Effect.all([seedStoreOrganization(ORGANIZATION), enrollStoreServer(ORGANIZATION)]).pipe(Effect.provide(services));
      const store = yield* cloudStore.pipe(Effect.provide(services));
      const write = (command: ConfigCommand) => Effect.promise(() => store.write(ORGANIZATION, command));
      yield* seedStoreGitService(ORGANIZATION, { project: PROJECT, environment: ENVIRONMENT, service: SERVICE }).pipe(Effect.provide(services));
      yield* write({ command: "publish", environment: here, version: null, accept_volume_loss: [] });
      yield* write({
        command: "set_pr_plan", project: null, repository: "acme/web", enabled: true, start_from: "production",
        copy: null, setup: null, remove_on_close: null, include_bots: null,
      });

      const runner: Parameters<typeof createStoreGithubPush>[1] = makeInngestEffectRunner((program) => Effect.runPromise(program.pipe(
        Effect.provide(services))));
      const inngest = new Inngest({ id: "store-github-merge-test" });
      const run = (after: string) => Effect.promise(async () =>
        (await new InngestTestEngine({
          function: createStoreGithubPush(inngest, runner),
          events: [push(after)],
          steps: [{ id: "dispatch", handler: () => undefined }],
        }).execute()).result as { admitted: string[] });

      expect((yield* run(H1)).admitted).toHaveLength(1);
      // Pull request 5 opens, and its PR Environment saves a change for production.
      yield* storeSystem(ORGANIZATION, {
        event: "pull_request", repository_id: 42, number: 5, title: "Add search", author: "ada", author_id: 7, bot: false,
        head_repository_id: 42, head_branch: "search", head: H2, target_branch: "main", commits: 1, open: true, merge_commit: null,
        target_head: null, updated: "2026-09-29T10:00:00Z",
      }).pipe(Effect.provide(services));
      const preview = { project: null, environment: "pr-5" };
      yield* write({ command: "edit", environment: preview, expect: null, changes: [{ op: "set", path: "web.env.SEARCH", value: "on" }] });
      yield* write({ command: "publish", environment: preview, version: null, accept_volume_loss: [] });

      // The branch moves past the merge commit; its push arrives before GitHub reports the merge.
      state.head = PAST;
      expect((yield* run(PAST)).admitted).toHaveLength(1);
      // The closed delivery never comes. The push's redelivery lands the merge, and the head it already saw carries it.
      state.merged = true;
      const landed = yield* run(PAST);
      expect(landed.admitted).toHaveLength(1);
      expect(yield* Effect.promise(() => store.read(ORGANIZATION, { query: "deployment", id: landed.admitted[0] ?? "" })))
        .toMatchObject({ builds: [{ commit: PAST }] });
    }),
  60_000,
);

it.live(
  "a push to a member's pull request deploys, and its builds pin, only while its author is a member",
  () =>
    Effect.gen(function* () {
      const state = { head: H2 };
      const services = yield* storeTestCloud({ github: fakeGithubApiBy(({ path }): JsonValue =>
        path.includes("/git/ref/")
          ? { ref: "refs/heads/search", object: { type: "commit", sha: state.head } }
          : path.includes("/compare/")
            ? { status: "ahead", files: [{ filename: "src/app.ts" }] }
            : { id: 42, full_name: "acme/web", private: true }).service });
      yield* Effect.all([seedStoreOrganization(ORGANIZATION), enrollStoreServer(ORGANIZATION)]).pipe(Effect.provide(services));
      const store = yield* cloudStore.pipe(Effect.provide(services));
      const write = (command: ConfigCommand) => Effect.promise(() => store.write(ORGANIZATION, command));
      yield* seedStoreGitService(ORGANIZATION, { project: PROJECT, environment: ENVIRONMENT, service: SERVICE }).pipe(Effect.provide(services));
      yield* write({ command: "publish", environment: here, version: null, accept_volume_loss: [] });
      yield* write({
        command: "set_pr_plan", project: null, repository: "acme/web", enabled: true, start_from: "production",
        copy: null, setup: null, remove_on_close: null, include_bots: null,
      });
      // ada, the member seeded with GitHub account 7, opens pull request 5 from acme/web itself.
      const opened = yield* storeSystem(ORGANIZATION, {
        event: "pull_request", repository_id: 42, number: 5, title: "Add search", author: "ada", author_id: 7, bot: false,
        head_repository_id: 42, head_branch: "search", head: H2, target_branch: "main", commits: 1, open: true, merge_commit: null,
        target_head: null, updated: "2026-09-29T10:00:00Z",
      }).pipe(Effect.provide(services));
      expect(opened.admitted).toHaveLength(1);
      const unlinkAda = Effect.gen(function* () {
        const { drizzle } = yield* Database;
        yield* drizzle.delete(account).where(eq(account.accountId, "7"));
      }).pipe(Effect.provide(services));
      yield* Effect.gen(function* () {
        const { drizzle } = yield* Database;
        yield* drizzle.insert(githubRepositoryCache).values({
          installationId: 7, repositoryId: 42, name: "web", fullName: "acme/web", defaultBranch: "main",
          private: true, htmlUrl: "https://github.com/acme/web", repoUpdatedAt: new Date(),
        });
      }).pipe(Effect.provide(services));

      const runner: Parameters<typeof createStoreGithubPush>[1] = makeInngestEffectRunner((program) => Effect.runPromise(program.pipe(
        Effect.provide(services))));
      const inngest = new Inngest({ id: "store-github-member-test" });
      const run = (before: string, after: string) => Effect.promise(async () =>
        (await new InngestTestEngine({
          function: createStoreGithubPush(inngest, runner),
          events: [{
            name: githubPushReceivedEvent,
            data: {
              kind: "push", installationId: 7, repositoryId: 42, ref: "refs/heads/search", branch: "search", beforeSha: before,
              afterSha: after, created: false, deleted: false, forced: false, deliveryId: `push-search-${after}`,
              branchKey: "7:42:refs/heads/search",
            },
          }],
          steps: [{ id: "dispatch", handler: () => undefined }],
        }).execute()).result as { admitted: string[] });

      state.head = H1;
      expect((yield* run(H2, H1)).admitted).toHaveLength(1);
      // A Deploy of pr-5 someone asks for pins its branch's head as ada's.
      const deploy = (id: string) => Effect.gen(function* () {
        yield* write({ command: "admit", admit: "deploy", id, environment: { project: null, environment: "pr-5" }, services: [], version: null, accept_volume_loss: [] });
        return yield* pinStoreSources(store, ORGANIZATION, id).pipe(Effect.provide(services), Effect.result);
      });
      const pinned = yield* deploy("00000000-0000-4000-8000-00000000b201");
      expect(pinned).toMatchObject({ _tag: "Success", success: [{ commit: H1 }] });

      // ada unlinks GitHub: Cloud no longer counts account 7 a member.
      yield* unlinkAda;
      state.head = MERGE;
      expect((yield* run(H1, MERGE)).admitted).toEqual([]);
      const refused = yield* deploy("00000000-0000-4000-8000-00000000b202");
      expect(refused).toMatchObject({ _tag: "Failure", failure: { _tag: "SourceUnreadable" } });
    }),
  60_000,
);
