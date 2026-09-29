import { testConfigEnvironment } from "#/test/config-environment";
import { it } from "@effect/vitest";
import { InngestTestEngine } from "@inngest/test";
import type { ConfigCommand, ConfigTrusted, ServiceId } from "@ployz/sdk";
import { ConfigProvider, Effect, Layer, Schema } from "effect";
import { Inngest } from "inngest";
import { expect } from "vitest";
import { cloudStore } from "#/modules/config-store/config-store.server";
import { createStorePullRequest } from "#/modules/config-store/store-pull-request.inngest";
import { GithubApi, type GithubApiService } from "#/modules/github/github-observation.api";
import { githubInstallation } from "#/modules/github/tables";
import { member, user } from "#/modules/identity/tables";
import { githubPullRequestReceivedEvent } from "#/modules/inngest/events";
import { organization } from "#/modules/organization/tables";
import { AppConfig } from "#/server/config.server";
import { Database, DatabaseLive } from "#/server/database.server";
import { makeInngestEffectRunner } from "#/server/run.server";
import { postgresTestDatabase } from "#/test/postgres";
import { SecretEncryptionLive } from "#/utils/encrypted-secret.server";

const ORGANIZATION = "00000000-0000-4000-8000-00000000c001";
const PROJECT = "00000000-0000-4000-8000-00000000c002";
const ENVIRONMENT = "00000000-0000-4000-8000-00000000c003";
const SERVICE = "00000000-0000-4000-8000-00000000c004";
const HEAD = "1".repeat(40);
const here = { project: null, environment: null };

type PullState = { state: "open" | "closed"; updated_at: string };

/** GitHub with `acme/web` (42) through installation 7: pull request 5 as `pull` says; check runs land in `posted`. */
function github(pull: PullState, posted: unknown[]): GithubApiService {
  return {
    json: (request) => {
      if (request.url.endsWith("/check-runs")) posted.push(request.body);
      const answer = request.url.endsWith("/pulls/5")
        ? {
          number: 5, title: "Add search", user: { login: "ada", type: "User" }, head: { ref: "search", sha: HEAD },
          base: { ref: "main" }, merged: false, merge_commit_sha: null, commits: 1, ...pull,
        }
        : request.url.includes("/check-runs?")
          ? { check_runs: [] }
          : request.url.endsWith("/check-runs") ? { id: 99 } : { id: 42, full_name: "acme/web", private: true };
      return Schema.decodeUnknownEffect(request.schema)(answer).pipe(Effect.orDie);
    },
    archive: () => Effect.die("no checkout in this test"),
  };
}

const delivery = (id: string) => ({
  name: githubPullRequestReceivedEvent,
  data: {
    kind: "pull_request", action: "opened", installationId: 7, repositoryId: 42, number: 5, headRepositoryId: 42,
    headSha: HEAD, deliveryId: id, pullRequestKey: "7:42:5",
  },
});

it.live(
  "pull requests reach the Store: a PR Environment deploys on open, its check is published, a late open never reopens it",
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

      const pull: PullState = { state: "open", updated_at: "2026-09-29T10:00:00Z" };
      const posted: unknown[] = [];
      const runner: Parameters<typeof createStorePullRequest>[1] = makeInngestEffectRunner((program) => Effect.runPromise(program.pipe(
        Effect.provide(services), Effect.provideService(GithubApi, github(pull, posted)))));
      const inngest = new Inngest({ id: "store-pull-request-test" });
      const dispatched: string[] = [];
      const run = (id: string) => Effect.promise(async () =>
        (await new InngestTestEngine({
          function: createStorePullRequest(inngest, runner),
          events: [delivery(id)],
          steps: [
            { id: "dispatch", handler: () => dispatched.push(id) },
            { id: "dispatch-removals", handler: () => dispatched.push(`${id}-removal`) },
          ],
        }).execute()).result as { admitted: string[]; removals: string[]; check: string });
      const environments = () => Effect.promise(async () => {
        const view = await store.read(ORGANIZATION, { query: "environments", project: null });
        return view.view === "environments" ? view.environments.map((listing) => listing.name) : [];
      });

      // Opened: the PR Environment deploys, and the check says nothing waits to be saved.
      const opened = yield* run("open");
      expect(opened.admitted).toHaveLength(1);
      expect(opened.check).toBe("posted");
      expect(dispatched).toEqual(["open"]);
      expect(yield* environments()).toEqual(["pr-5", "production"]);
      expect(posted).toMatchObject([{ head_sha: HEAD, conclusion: "success", output: { title: "No changes for production" } }]);

      // Closed: never deployed, so it goes at once; no check for a closed pull request.
      Object.assign(pull, { state: "closed", updated_at: "2026-09-29T11:00:00Z" });
      expect((yield* run("close")).check).toBe("skipped");
      expect(yield* environments()).toEqual(["production"]);

      // GitHub's older open, read late, never brings it back.
      Object.assign(pull, { state: "open", updated_at: "2026-09-29T10:30:00Z" });
      expect((yield* run("late")).admitted).toEqual([]);
      expect(yield* environments()).toEqual(["production"]);

      // Reopened, it runs (and fails: its source can't be read); closed again, Cloud takes it off the Servers first.
      Object.assign(pull, { state: "open", updated_at: "2026-09-29T12:00:00Z" });
      const reopened = yield* run("reopen");
      yield* Effect.promise(() => store.runDeployment(ORGANIZATION, reopened.admitted[0] ?? "", "cloud-test", [], { failure: "No source." }));
      Object.assign(pull, { state: "closed", updated_at: "2026-09-29T13:00:00Z" });
      const closed = yield* run("close-again");
      expect(closed.removals).toHaveLength(1);
      expect(dispatched).toContain("close-again-removal");
      expect(yield* Effect.promise(() => store.read(ORGANIZATION, { query: "deployment", id: closed.removals[0] ?? "" })))
        .toMatchObject({ remove: true, status: "queued" });
    }),
  60_000,
);
