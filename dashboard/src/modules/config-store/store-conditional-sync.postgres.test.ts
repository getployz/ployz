import { it } from "@effect/vitest";
import { InngestTestEngine } from "@inngest/test";
import type { ConfigCommand, JsonValue, ConfigTrusted, SystemEvent } from "@ployz/sdk";
import { Effect, Layer } from "effect";
import { Inngest } from "inngest";
import { expect } from "vitest";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import { createStoreGithubPush } from "#/modules/config-store/store-github.inngest";
import { githubPushReceivedEvent } from "#/modules/inngest/events";
import { makeInngestEffectRunner } from "#/server/run.server";
import { fakeGithubApiBy } from "#/test/fake-github";
import { enrollStoreServer, seedStoreOrganization, seedStoreGitService, storeTestCloud } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000d001";
const PROJECT = "00000000-0000-4000-8000-00000000d002";
const ENVIRONMENT = "00000000-0000-4000-8000-00000000d003";
const SERVICE = "00000000-0000-4000-8000-00000000d004";
const PR_HEAD = "1".repeat(40);
const MERGE = "3".repeat(40);
const HEAD = "4".repeat(40);
const here = { project: null, environment: null };

/** GitHub with `acme/web` (42) through installation 7: pull request 5 merged as MERGE, `main` at HEAD, which has it. */
const github = fakeGithubApiBy(({ url }): JsonValue => url.endsWith("/pulls/5")
  ? {
    number: 5, title: "Add search", user: { login: "ada", type: "User" }, head: { ref: "search", sha: PR_HEAD },
    base: { ref: "main" }, state: "closed", merged: true, merge_commit_sha: MERGE, commits: 1,
    updated_at: "2026-09-29T11:00:00Z",
  }
  : url.includes("/git/ref/")
    ? { ref: "refs/heads/main", object: { type: "commit", sha: HEAD } }
    : url.includes("/compare/")
      ? { status: "ahead", files: [{ filename: "src/app.ts" }] }
      : { id: 42, full_name: "acme/web", private: true }).service;

it.live(
  "a merge pushed before its closed delivery freezes the Conditional Sync, and that push lands it before it deploys",
  () =>
    Effect.gen(function* () {
      const services = yield* Layer.build(yield* storeTestCloud({ github }));
      yield* Effect.all([seedStoreOrganization(ORGANIZATION), enrollStoreServer(ORGANIZATION)]).pipe(Effect.provide(services));
      const store = yield* cloudStore.pipe(Effect.provide(services));
      const write = (command: ConfigCommand, trusted?: ConfigTrusted) =>
        Effect.promise(() => store.write(ORGANIZATION, command, trusted));
      yield* seedStoreGitService(ORGANIZATION, { project: PROJECT, environment: ENVIRONMENT, service: SERVICE }).pipe(Effect.provide(services));
      yield* write({ command: "publish", environment: here, version: null, accept_volume_loss: [] });
      yield* write({
        command: "set_pr_plan", project: null, repository: "acme/web", enabled: true, start_from: "production",
        copy: null, setup: null, remove_on_close: null, include_bots: null,
      });
      const opened: SystemEvent = {
        event: "pull_request", repository_id: 42, number: 5, title: "Add search", author: "ada", bot: false,
        head_branch: "search", head: PR_HEAD, target_branch: "main", commits: 1, open: true, merge_commit: null,
        merge_reached: null, updated: "2026-09-29T10:00:00Z",
      };
      yield* Effect.promise(() => store.system(ORGANIZATION, opened));

      // PR #5 changes a variable and syncs it for its merge; its check is named for Cloud to publish.
      const pr = { project: null, environment: "pr-5" };
      yield* write({ command: "edit", environment: pr, expect: null, changes: [{ op: "set", path: "web.env.MODE", value: "fast" }] });
      const view = yield* Effect.promise(() => store.read(ORGANIZATION, { query: "sync", from: pr, when: "at_merge" }));
      const synced = yield* write({ command: "sync", from: pr, when: "at_merge", version: view.version });
      expect(synced).toMatchObject({
        written: "synced", staged: [], conditional_sync: { state: "standing", rows: [{ node: "web", name: "env.MODE" }] },
        checks: [{ repository_id: 42, number: 5 }],
      });

      // GitHub delivers the merge push before the closed pull request.
      const runner: Parameters<typeof createStoreGithubPush>[1] = makeInngestEffectRunner((program) => Effect.runPromise(program.pipe(
        Effect.provide(services))));
      const pushed = (yield* Effect.promise(async () => (await new InngestTestEngine({
        function: createStoreGithubPush(new Inngest({ id: "store-conditional-sync-test" }), runner),
        events: [{
          name: githubPushReceivedEvent,
          data: {
            kind: "push", installationId: 7, repositoryId: 42, ref: "refs/heads/main", branch: "main", beforeSha: PR_HEAD,
            afterSha: HEAD, created: false, deleted: false, forced: false, deliveryId: "push-merge", branchKey: "7:42:refs/heads/main",
          },
        }],
        steps: [{ id: "dispatch", handler: () => undefined }],
      }).execute()).result)) as { admitted: string[] };
      expect(pushed.admitted).toHaveLength(1);
      const web = yield* Effect.promise(() => store.read(ORGANIZATION, { query: "service", environment: here, service: "web" }));
      expect(web).toMatchObject({ values: { env: { MODE: "fast" } } });
      expect(yield* Effect.promise(() => store.pendingSyncs(ORGANIZATION, 42, "main"))).toEqual({ standing: [], merged: [] });
      const environments = yield* Effect.promise(() => store.read(ORGANIZATION, { query: "environments", project: null }));
      expect(environments.environments.map((listing) => listing.name)).toEqual(["production"]);
    }),
  60_000,
);
