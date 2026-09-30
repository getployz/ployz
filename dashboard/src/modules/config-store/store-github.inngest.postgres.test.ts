import { it } from "@effect/vitest";
import { InngestTestEngine } from "@inngest/test";
import type { ConfigCommand, JsonValue, ConfigTrusted } from "@ployz/sdk";
import { Effect, Layer } from "effect";
import { Inngest } from "inngest";
import { expect } from "vitest";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import { createStoreGithubCheckSuite, createStoreGithubPush } from "#/modules/config-store/store-github.inngest";
import { githubCheckSuiteReceivedEvent, githubPushReceivedEvent } from "#/modules/inngest/events";
import { makeInngestEffectRunner } from "#/server/run.server";
import { fakeGithubApiBy } from "#/test/fake-github";
import { enrollStoreServer, seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000b001";
const PROJECT = "00000000-0000-4000-8000-00000000b002";
const ENVIRONMENT = "00000000-0000-4000-8000-00000000b003";
const SERVICE = "00000000-0000-4000-8000-00000000b004";
const H1 = "1".repeat(40);
const H2 = "2".repeat(40);
const here = { project: null, environment: null };

/** GitHub with `acme/web` (42) through installation 7: `main` at `state.head`, suite 9 as `state.suite` says. */
function github(state: { head: string; suite: { status: string; conclusion: string | null; updated_at: string } }) {
  return fakeGithubApiBy(({ url }): JsonValue => url.includes("/git/ref/")
    ? { ref: "refs/heads/main", object: { type: "commit", sha: state.head } }
    : url.includes("/compare/")
      ? { status: "ahead", files: [{ filename: "src/app.ts" }] }
      : url.includes("/check-suites/")
        ? { id: 9, head_sha: H2, ...state.suite }
        : { id: 42, full_name: "acme/web", private: true }).service;
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
      const services = yield* Layer.build(yield* storeTestCloud({ github: github(state) }));
      // A member of the Organization installed the GitHub App as installation 7.
      yield* Effect.all([seedStoreOrganization(ORGANIZATION), enrollStoreServer(ORGANIZATION)]).pipe(Effect.provide(services));
      const store = yield* cloudStore.pipe(Effect.provide(services));
      const write = (command: ConfigCommand, trusted?: ConfigTrusted) =>
        Effect.promise(() => store.write(ORGANIZATION, command, trusted));
      yield* write({ command: "create_project", id: PROJECT, name: "shop", default_environment: ENVIRONMENT });
      yield* write({ command: "create_git_service", id: SERVICE, environment: here, name: "web", repository: "acme/web", branch: null }, {
        repositories: [{
          repository: "acme/web", repository_id: 42, access: { type: "github-installation", installationId: 7 },
          default_branch: "main", branches: [],
        }],
        domains: { custom_domains: false, cluster_domain: null, certificates: null, ingress_addresses: [], lookups: [] },
      });
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
