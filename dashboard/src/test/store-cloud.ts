import { createHash } from "node:crypto";
import type { ConfigTrusted, MachineId } from "@ployz/sdk";
import { ConfigProvider, Effect, Layer } from "effect";
import { Inngest } from "inngest";
import { Polar, type PolarService } from "#/modules/billing/polar-provider.server";
import { cloudStore, CloudStoreLive } from "#/modules/config-store/store-sdk.server";
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
import { organizationMachine } from "#/modules/machines/tables";
import { organizationPairing } from "#/modules/runtime/tables";
import { SecretEncryption, SecretEncryptionLive } from "#/utils/encrypted-secret.server";

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

/** Pair Organization `id` with Cloud and enroll one Server, unreachable, so the Store admits Deployments there. */
export const enrollStoreServer = Effect.fn(function* (id: string) {
  const { drizzle } = yield* Database;
  const encryption = yield* SecretEncryption;
  // SAFETY: 32 lowercase hex digits, the Machine ID representation.
  const machineId = "0".repeat(32) as MachineId;
  const secret = "ppair_fixture_store";
  yield* drizzle.insert(organizationPairing).values({
    organizationId: id, encryptedPairingSecret: encryption.encrypt(secret), founderPublicKey: "founder-key", founderClaimMachineId: machineId,
  });
  yield* drizzle.insert(organizationMachine).values({
    organizationId: id, machineId, clusterKey: createHash("sha256").update(secret).digest("hex"),
    encryptedCapability: encryption.encrypt(`ployz1:cloud:${machineId}`), isDialEntry: true,
  });
});

/** What Cloud observes of `acme/web` (42): readable through installation 7, or public. */
export const acmeWeb = (access: "installation" | "public" = "installation"): ConfigTrusted => ({
  repositories: [{
    repository: "acme/web", repository_id: 42, default_branch: "main", branches: [],
    access: access === "public" ? { type: "public" } : { type: "github-installation", installationId: 7 },
  }],
  domains: { custom_domains: false, cluster_domain: null, certificates: null, ingress_addresses: [], lookups: [] },
});

/**
 * In Organization `organization`: Project `shop` (default Environment `ids.environment`) with Git Service `web` on
 * `acme/web`. Resolves to the Store.
 */
export const seedStoreGitService = Effect.fn(function* (
  organization: string, ids: { project: string; environment: string; service: string }, access: "installation" | "public" = "installation",
) {
  const store = yield* cloudStore;
  const here = { project: null, environment: null };
  yield* Effect.promise(() => store.write(organization, { command: "create_project", id: ids.project, name: "shop", default_environment: ids.environment }));
  yield* Effect.promise(() => store.write(organization, {
    command: "create_git_service", id: ids.service, environment: here, name: "web", repository: "acme/web", branch: null,
  }, acmeWeb(access)));
  return store;
});
