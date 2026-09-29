import { testConfigEnvironment } from "#/test/config-environment";
import { it } from "@effect/vitest";
import { InngestTestEngine } from "@inngest/test";
import type { ConfigCommand, ConfigTrusted, ServiceId, SystemEvent } from "@ployz/sdk";
import { ConfigProvider, Effect, Layer, Schema } from "effect";
import { Inngest } from "inngest";
import { expect } from "vitest";
import { cloudStore } from "#/modules/config-store/config-store.server";
import { createStoreGithubPush } from "#/modules/config-store/store-github.inngest";
import { GithubApi, type GithubApiService } from "#/modules/github/github-observation.api";
import { githubInstallation } from "#/modules/github/tables";
import { member, user } from "#/modules/identity/tables";
import { githubPushReceivedEvent } from "#/modules/inngest/events";
import { organization } from "#/modules/organization/tables";
import { AppConfig } from "#/server/config.server";
import { Database, DatabaseLive } from "#/server/database.server";
import { makeInngestEffectRunner } from "#/server/run.server";
import { postgresTestDatabase } from "#/test/postgres";
import { SecretEncryptionLive } from "#/utils/encrypted-secret.server";

const ORGANIZATION = "00000000-0000-4000-8000-00000000d001";
const PROJECT = "00000000-0000-4000-8000-00000000d002";
const ENVIRONMENT = "00000000-0000-4000-8000-00000000d003";
const SERVICE = "00000000-0000-4000-8000-00000000d004";
const PR_HEAD = "1".repeat(40);
const MERGE = "3".repeat(40);
const HEAD = "4".repeat(40);
const here = { project: null, environment: null };

/** GitHub with `acme/web` (42) through installation 7: pull request 5 merged as MERGE, `main` at HEAD, which has it. */
const github: GithubApiService = {
  json: (request) => {
    const answer = request.url.endsWith("/pulls/5")
      ? {
        number: 5, title: "Add search", user: { login: "ada", type: "User" }, head: { ref: "search", sha: PR_HEAD },
        base: { ref: "main" }, state: "closed", merged: true, merge_commit_sha: MERGE, commits: 1,
        updated_at: "2026-09-29T11:00:00Z",
      }
      : request.url.includes("/git/ref/")
        ? { ref: "refs/heads/main", object: { type: "commit", sha: HEAD } }
        : request.url.includes("/compare/")
          ? { status: "ahead", files: [{ filename: "src/app.ts" }] }
          : { id: 42, full_name: "acme/web", private: true };
    return Schema.decodeUnknownEffect(request.schema)(answer).pipe(Effect.orDie);
  },
  archive: () => Effect.die("no checkout in this test"),
};

it.live(
  "a merge pushed before its closed delivery freezes the Conditional Save, and that push lands it before it deploys",
  () =>
    Effect.gen(function* () {
      const cloud = yield* postgresTestDatabase;
      const env = { ...testConfigEnvironment(), NODE_ENV: "test", DATABASE_URL: cloud.url.href };
      const configLayer = AppConfig.layer.pipe(Layer.provide(ConfigProvider.layer(ConfigProvider.fromEnv({ env }))));
      const services = yield* Layer.build(Layer.mergeAll(
        configLayer,
        DatabaseLive.pipe(Layer.provide(configLayer)),
        SecretEncryptionLive.pipe(Layer.provide(configLayer)),
      ));
      yield* Effect.gen(function* () {
        const { drizzle } = yield* Database;
        const [owner] = yield* drizzle.insert(user).values({ email: "ada@example.test", name: "Ada" }).returning();
        yield* drizzle.insert(organization).values({ id: ORGANIZATION, name: "Shop", slug: "shop" });
        yield* drizzle.insert(member).values({ userId: owner?.id ?? "", organizationId: ORGANIZATION, role: "owner" });
        yield* drizzle.insert(githubInstallation).values({
          userId: owner?.id ?? "", installationId: 7, accountLogin: "acme", accountType: "Organization",
        });
      }).pipe(Effect.provide(services));

      const store = yield* cloudStore.pipe(Effect.provide(services));
      const write = (command: ConfigCommand, trusted?: ConfigTrusted) =>
        Effect.promise(() => store.write(ORGANIZATION, command, trusted));
      yield* write({ command: "create_project", id: PROJECT, name: "shop", default_environment: ENVIRONMENT });
      yield* write({ command: "create_git_service", id: SERVICE as ServiceId, environment: here, name: "web", repository: "acme/web", branch: null }, {
        repositories: [{
          repository: "acme/web", repository_id: 42, access: { type: "github-installation", installationId: 7 },
          default_branch: "main", branches: [],
        }],
        domains: { custom_domains: false, cluster_domain: null, certificates: null, ingress_addresses: [], lookups: [] },
      });
      yield* write({ command: "publish", environment: here, version: null });
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

      // PR #5 changes a variable and saves it for its merge; its check is named for Cloud to publish.
      const pr = { project: null, environment: "pr-5" };
      yield* write({ command: "edit", environment: pr, expect: null, changes: [{ op: "set", path: "web.env.MODE", value: "fast" }] });
      const saved = yield* write({ command: "move", from: pr });
      expect(saved).toMatchObject({
        written: "moved", staged: [], conditional_save: { state: "standing", rows: ["web.env.MODE"] },
        checks: [{ repository_id: 42, number: 5 }],
      });

      // GitHub delivers the merge push before the closed pull request.
      const runner: Parameters<typeof createStoreGithubPush>[1] = makeInngestEffectRunner((program) => Effect.runPromise(program.pipe(
        Effect.provide(services), Effect.provideService(GithubApi, github))));
      const pushed = (yield* Effect.promise(async () => (await new InngestTestEngine({
        function: createStoreGithubPush(new Inngest({ id: "store-conditional-save-test" }), runner),
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
      expect(yield* Effect.promise(() => store.pendingSaves(ORGANIZATION, 42, "main"))).toEqual({ standing: [], merged: [] });
      const environments = yield* Effect.promise(() => store.read(ORGANIZATION, { query: "environments", project: null }));
      expect(environments.view === "environments" && environments.environments.map((listing) => listing.name)).toEqual(["production"]);
    }),
  60_000,
);
