import { assert, it } from "@effect/vitest";
import { ConfigProvider, Effect, Layer } from "effect";
import { vi } from "vitest";
import { account, githubInstallation, githubRepositoryCache, user } from "#/db/schema";
import { executeScheduleGithubRepositorySync } from "#/modules/github/inngest-sync/scheduled";
import {
  executeSyncGithubRepositories,
  SYNC_GITHUB_REPOSITORIES_SINGLETON,
  type GithubSyncEffectRunner,
} from "#/modules/github/inngest-sync/sync";
import { executeProcessGithubInstallationReceived } from "#/modules/github/inngest-sync/webhook";
import { GithubApiLive } from "#/modules/github/github-observation.api";
import { AppConfig } from "#/server/config.server";
import { Database, DatabaseLive } from "#/server/database.server";
import type { PloyzStepTools } from "#/modules/inngest/client";
import { githubInstallationReceivedEvent } from "#/modules/inngest/events";
import {
  migrateTestDatabase,
  postgresTestContainer,
} from "#/test/postgres";

function createStepTools() {
  const run: PloyzStepTools["run"] = async (_id, operation, ...input) => {
    const result = await operation(...input);
    return result === undefined ? null : JSON.parse(JSON.stringify(result));
  };
  return {
    run,
    sendEvent: vi.fn(async () => ({ ids: [] })),
  };
}

function jsonResponse<Body>(body: Body) {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { "content-type": "application/json" },
  });
}

it.live(
  "persists decoded repository pages and schedules sync with stable Inngest ABI",
  () =>
    Effect.gen(function* () {
      const container = yield* postgresTestContainer;
      yield* migrateTestDatabase(container.url);
      const config = AppConfig.layer.pipe(
        Layer.provide(
          ConfigProvider.layer(
            ConfigProvider.fromEnv({
              env: {
                ...process.env,
                DATABASE_URL: container.url.href,
                APP_URL: "http://localhost:3000",
                BETTER_AUTH_SECRET: "better-auth-secret",
                GITHUB_CLIENT_ID: "github-client-id",
                GITHUB_CLIENT_SECRET: "github-client-secret",
                APP_ENCRYPTION_SECRET:
                  "app-encryption-secret-at-least-32-characters",
              },
            }),
          ),
        ),
      );
      const layer = DatabaseLive.pipe(
        Layer.provide(config),
        Layer.merge(config),
        Layer.provideMerge(GithubApiLive.pipe(Layer.provideMerge(config))),
      );
      const runEffect: GithubSyncEffectRunner = (program) =>
        Effect.runPromise(program.pipe(Effect.provide(layer)));

      yield* Effect.promise(() =>
        runEffect(
          Effect.gen(function* () {
            const database = yield* Database;
            const users = yield* database.drizzle
              .insert(user)
              .values({
                email: "github-sync@example.test",
                emailVerified: true,
                name: "GitHub Sync",
              })
              .returning({ id: user.id });
            const record = users[0];
            if (record === undefined) return yield* Effect.die("Missing user");
            yield* database.drizzle.insert(account).values({
              accountId: "4242",
              providerId: "github",
              userId: record.id,
            });
          }),
        ),
      );

      const installationStep = createStepTools();
      const installationResult = yield* Effect.promise(() =>
        executeProcessGithubInstallationReceived(
          {
            event: {
              name: githubInstallationReceivedEvent,
              data: {
                deliveryId: "delivery-installation-1",
                action: "created",
                installation: {
                  id: 1717,
                  account: {
                    login: "acme",
                    type: "Organization",
                    avatar_url: "https://github.com/acme.png",
                  },
                },
                sender: { id: 4242, login: "nick" },
              },
            },
            step: installationStep,
          },
          runEffect,
        ),
      );
      assert.deepStrictEqual(installationResult, {
        installationId: 1717,
        shouldSyncRepositories: true,
      });
      assert.deepStrictEqual(installationStep.sendEvent.mock.calls, [
        [
          "request-repository-sync",
          {
            name: "github/repositories-sync.requested",
            data: { installationId: 1717, reason: "installation.created" },
          },
        ],
      ]);

      const fetchMock = vi
        .fn<typeof fetch>()
        .mockResolvedValueOnce(
          jsonResponse({
            token: "installation-secret",
            expires_at: "2099-01-01T00:00:00Z",
          }),
        )
        .mockResolvedValueOnce(
          jsonResponse({
            total_count: 1,
            repositories: [
              {
                id: 9191,
                name: "api",
                full_name: "acme/api",
                private: true,
                default_branch: "main",
                html_url: "https://github.com/acme/api",
                updated_at: "2026-03-27T00:00:00.000Z",
                provider_only: "removed",
              },
            ],
          }),
        );
      vi.stubGlobal("fetch", fetchMock);
      yield* Effect.addFinalizer(() =>
        Effect.sync(() => vi.unstubAllGlobals()),
      );

      const step = createStepTools();
      const result = yield* Effect.promise(() =>
        executeSyncGithubRepositories(
          {
            event: {
              data: { installationId: 1717, reason: "manual-refresh" },
            },
            step,
          },
          runEffect,
        ),
      );
      assert.deepStrictEqual(result, {
        installationId: 1717,
        processedPageCount: 1,
        processedRepoCount: 1,
        deletedRepositoryCount: 0,
      });
      const repositories = yield* Effect.promise(() =>
        runEffect(
          Effect.gen(function* () {
            const database = yield* Database;
            return yield* database.drizzle
              .select({
                repositoryId: githubRepositoryCache.repositoryId,
                name: githubRepositoryCache.name,
                fullName: githubRepositoryCache.fullName,
              })
              .from(githubRepositoryCache);
          }),
        ),
      );
      assert.deepStrictEqual(repositories, [
        { repositoryId: 9191, name: "api", fullName: "acme/api" },
      ]);

      const installations = yield* Effect.promise(() =>
        runEffect(
          Effect.gen(function* () {
            const database = yield* Database;
            return yield* database.drizzle
              .select({ installationId: githubInstallation.installationId })
              .from(githubInstallation);
          }),
        ),
      );
      assert.deepStrictEqual(installations, [{ installationId: 1717 }]);

      const scheduleStep = createStepTools();
      const scheduled = yield* Effect.promise(() =>
        executeScheduleGithubRepositorySync(
          { step: scheduleStep },
          runEffect,
        ),
      );
      assert.deepStrictEqual(scheduled, { installationCount: 1 });
      assert.deepStrictEqual(scheduleStep.sendEvent.mock.calls, [
        [
          "request-repository-syncs",
          [
            {
              name: "github/repositories-sync.requested",
              data: { installationId: 1717, reason: "scheduled" },
            },
          ],
        ],
      ]);
      assert.deepStrictEqual(SYNC_GITHUB_REPOSITORIES_SINGLETON, {
        key: "event.data.installationId",
        mode: "skip",
      });
    }),
  60_000,
);
