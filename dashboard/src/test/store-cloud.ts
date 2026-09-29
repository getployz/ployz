import { ConfigProvider, Effect, Layer } from "effect";
import { Inngest } from "inngest";
import { Polar, type PolarService } from "#/modules/billing/polar-provider.server";
import { CloudStoreLive } from "#/modules/config-store/store-sdk.server";
import { GithubApi, type GithubApiService } from "#/modules/github/github-observation.api";
import { githubInstallation } from "#/modules/github/tables";
import { member, user } from "#/modules/identity/tables";
import { InngestClient } from "#/modules/inngest/client";
import { organization } from "#/modules/organization/tables";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { AppConfig } from "#/server/config.server";
import { Database, DatabaseLive } from "#/server/database.server";
import { testConfigEnvironment } from "#/test/config-environment";
import { fakeGithubApi } from "#/test/fake-github";
import { postgresTestDatabase } from "#/test/postgres";
import { SecretEncryptionLive } from "#/utils/encrypted-secret.server";

/**
 * Cloud with the Config Store in a fresh database, and everything its Store workers use: GitHub as `github` answers
 * (none by default), no Cluster paired, a self-hosted billing plan, and an Inngest client that sends nowhere. `env`
 * overrides configuration.
 */
export const storeTestCloud = Effect.fn(function* (options: {
  readonly github?: GithubApiService;
  readonly polar?: PolarService;
  readonly inngest?: Inngest;
  readonly env?: Readonly<Record<string, string>>;
} = {}) {
  const cloud = yield* postgresTestDatabase;
  const env = { ...testConfigEnvironment(), NODE_ENV: "test", DATABASE_URL: cloud.url.href, ...options.env };
  const configLayer = AppConfig.layer.pipe(Layer.provide(ConfigProvider.layer(ConfigProvider.fromEnv({ env }))));
  const database = DatabaseLive.pipe(Layer.provide(configLayer));
  return Layer.mergeAll(
    configLayer,
    database,
    SecretEncryptionLive.pipe(Layer.provide(configLayer)),
    CloudStoreLive.pipe(Layer.provide(Layer.merge(configLayer, database))),
    Layer.succeed(GithubApi, options.github ?? fakeGithubApi().service),
    Layer.succeed(Polar, options.polar ?? { mode: "self_hosted" }),
    Layer.succeed(InngestClient, options.inngest ?? new Inngest({ id: "store-test" })),
    // No Cluster is paired: domains read as unobserved.
    Layer.succeed(OrganizationRuntime, { cancel: () => Effect.void, open: () => Effect.succeed({ status: "no_connection" as const }) }),
  );
});

/** Organization `id` (slug `shop`), whose one member Ada installed the GitHub App on `acme` as installation 7. */
export const seedStoreOrganization = Effect.fn(function* (id: string) {
  const { drizzle } = yield* Database;
  const [owner] = yield* drizzle.insert(user).values({ email: "ada@example.test", name: "Ada" }).returning();
  const userId = owner?.id ?? "";
  yield* drizzle.insert(organization).values({ id, name: "Shop", slug: "shop" });
  yield* drizzle.insert(member).values({ userId, organizationId: id, role: "owner" });
  yield* drizzle.insert(githubInstallation).values({ userId, installationId: 7, accountLogin: "acme", accountType: "Organization" });
  return userId;
});
