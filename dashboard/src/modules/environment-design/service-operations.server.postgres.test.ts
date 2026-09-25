import { testConfigEnvironment } from "#/test/config-environment";
import { editServiceMetadata } from "./service-metadata.server";
import { loadEnvironmentDocument, loadCurrentEnvironmentState } from "./working-state-repository.server";
import { emptyEnvironmentIntent } from "./saved-intent";
import { assert, it } from "@effect/vitest";
import type { MachineId } from "@ployz/sdk";
import { asTestDouble } from "#/lib/test-double";
import { randomUUID } from "node:crypto";
import { sql } from "drizzle-orm";
import { ConfigProvider, Effect, Layer } from "effect";
import { Polar, type PolarService } from "#/modules/billing/polar-provider.server";
import {
  environment,
  member,
  organization,
  organizationBillingState,
  project,
  user,
} from "#/db/schema";
import { AppConfig } from "#/server/config.server";
import { Database, DatabaseLive } from "#/server/database.server";
import { SecretEncryptionLive } from "#/utils/encrypted-secret.server";
import {
  migrateTestDatabase,
  postgresTestContainer,
} from "#/test/postgres";
import {
  clearServiceRegistryCredential,
  createService,
  restoreServiceRegistryCredential,
  setServiceRegistryCredential,
  updateService,
  updateServiceCanvasPosition,
} from "./service-operations.server";
import { createGitServiceSource, createImageServiceSource } from "./services";

it.live(
  "keeps service, credential, and canvas authoring authorized and atomic",
  () =>
    Effect.gen(function* () {
      const container = yield* postgresTestContainer;
      yield* migrateTestDatabase(container.url);
      const config = AppConfig.layer.pipe(
        Layer.provide(
          ConfigProvider.layer(
            ConfigProvider.fromEnv({
              env: {
                ...testConfigEnvironment(),
                DATABASE_URL: container.url.href,
              },
            }),
          ),
        ),
      );
      const layer = Layer.merge(
        DatabaseLive.pipe(Layer.provide(config)),
        Layer.succeed(Polar, { mode: "self_hosted" }),
      ).pipe(
        Layer.merge(SecretEncryptionLive.pipe(Layer.provide(config))),
      );

      yield* Effect.gen(function* () {
        const database = yield* Database;
        const users = yield* database.drizzle
          .insert(user)
          .values([
            { email: "service@example.test", emailVerified: true, name: "Service" },
            { email: "outsider@example.test", emailVerified: true, name: "Outsider" },
          ])
          .returning({ id: user.id });
        const author = users[0];
        const outsider = users[1];
        if (author === undefined || outsider === undefined) {
          return yield* Effect.die("PostgreSQL did not return the test users.");
        }
        const organizations = yield* database.drizzle
          .insert(organization)
          .values({ name: "Acme", slug: "acme" })
          .returning({ id: organization.id });
        const organizationRecord = organizations[0];
        if (organizationRecord === undefined) {
          return yield* Effect.die("PostgreSQL did not return the organization.");
        }
        yield* database.drizzle.insert(member).values({
          userId: author.id,
          organizationId: organizationRecord.id,
          role: "owner",
        });
        const projects = yield* database.drizzle
          .insert(project)
          .values({ organizationId: organizationRecord.id, name: "API", slug: "api" })
          .returning({ id: project.id });
        const projectRecord = projects[0];
        if (projectRecord === undefined) {
          return yield* Effect.die("PostgreSQL did not return the project.");
        }
        const environments = yield* database.drizzle
          .insert(environment)
          .values({
            organizationId: organizationRecord.id,
            projectId: projectRecord.id,
            name: "Production",
            namespace: "api-production",
            intent: emptyEnvironmentIntent("api-production"),
          })
          .returning({ id: environment.id });
        const environmentRecord = environments[0];
        if (environmentRecord === undefined) {
          return yield* Effect.die("PostgreSQL did not return the environment.");
        }
        const actor = { userId: author.id };

        const created = yield* createService(actor, {
          organizationSlug: "acme",
          environmentId: environmentRecord.id,
          name: "Registry",
          source: createImageServiceSource({ image: "acme/api:latest" }),
          x: 10.4,
          y: 20.6,
          preDeployCommand: null,
          startCommand: null,
          healthcheck: { type: "none" },
          restartPolicy: "unless-stopped",
        });
        const committed = yield* database.drizzle.execute<{ txid: string }>(
          sql`
            select xmin::text as txid from service
            where id = ${created.data.service.id}
            union all
            select xmin::text as txid from environment_canvas_node_position
            where resource_id = ${created.data.service.id}
          `,
          "objects",
        );
        assert.strictEqual(committed.length, 2);
        assert.strictEqual(new Set(committed.map((row) => row.txid)).size, 1);

        const updated = yield* updateService(actor, {
          organizationSlug: "acme",
          environmentId: environmentRecord.id,
          serviceId: created.data.service.id,
          managedHostnames: [{ prefix: "public-api", targetPort: 4000 }],
          revision: (yield* loadEnvironmentDocument(environmentRecord.id)).revision,
        });
        const metadata = yield* editServiceMetadata(actor, {
          organizationSlug: "acme", environmentId: environmentRecord.id, serviceId: created.data.service.id,
          edit: { kind: "rename", name: "Registry API" },
        });
        assert.strictEqual(metadata.data.name, "Registry API");
        assert.strictEqual((yield* loadEnvironmentDocument(environmentRecord.id)).revision, updated.data.revision);
        assert.strictEqual((yield* loadEnvironmentDocument(environmentRecord.id)).intent.services[0]?.config.privateDns, created.data.service.privateDns);
        const beforePolicy = yield* loadEnvironmentDocument(environmentRecord.id);
        yield* editServiceMetadata(actor, { organizationSlug: "acme", environmentId: environmentRecord.id, serviceId: created.data.service.id,
          edit: { kind: "policy", policy: { autoDeploy: false, watchPaths: ["src/**"] } } });
        assert.deepStrictEqual(yield* loadEnvironmentDocument(environmentRecord.id), beforePolicy);
        // The Preferred Builder is policy too: set and cleared back to Auto without touching the document.
        const preferred = yield* editServiceMetadata(actor, { organizationSlug: "acme", environmentId: environmentRecord.id, serviceId: created.data.service.id,
          edit: { kind: "policy", policy: { preferredBuilder: asTestDouble<MachineId>()("a".repeat(32)) } } });
        assert.deepStrictEqual(preferred.data.policy, { autoDeploy: false, waitForCi: false, watchPaths: ["src/**"], imageUpdate: { type: "off" }, preferredBuilder: asTestDouble<MachineId>()("a".repeat(32)) });
        const auto = yield* editServiceMetadata(actor, { organizationSlug: "acme", environmentId: environmentRecord.id, serviceId: created.data.service.id,
          edit: { kind: "policy", policy: { preferredBuilder: null } } });
        assert.deepStrictEqual(auto.data.policy, { autoDeploy: false, waitForCi: false, watchPaths: ["src/**"], imageUpdate: { type: "off" } });
        assert.deepStrictEqual(yield* loadEnvironmentDocument(environmentRecord.id), beforePolicy);
        assert.deepStrictEqual((yield* loadCurrentEnvironmentState(environmentRecord.id)).intent.services[0]?.config.managedHostnames, [{ prefix: "public-api", targetPort: 4000 }]);

        yield* updateServiceCanvasPosition(actor, {
          organizationSlug: "acme",
          environmentId: environmentRecord.id,
          serviceId: created.data.service.id,
          x: 30.2,
          y: 40.8,
        });
        yield* setServiceRegistryCredential(actor, {
          revision: (yield* loadEnvironmentDocument(environmentRecord.id)).revision,
          organizationSlug: "acme",
          environmentId: environmentRecord.id,
          serviceId: created.data.service.id,
          username: "octocat",
          secret: "secret-token",
        });
        yield* clearServiceRegistryCredential(actor, {
          revision: (yield* loadEnvironmentDocument(environmentRecord.id)).revision,
          organizationSlug: "acme",
          environmentId: environmentRecord.id,
          serviceId: created.data.service.id,
        });
        yield* restoreServiceRegistryCredential(actor, {
          revision: (yield* loadEnvironmentDocument(environmentRecord.id)).revision,
          organizationSlug: "acme",
          environmentId: environmentRecord.id,
          serviceId: created.data.service.id,
        });


        const publicService = yield* createService(actor, {
          organizationSlug: "acme", environmentId: environmentRecord.id, name: "Public",
          source: createGitServiceSource({ repository: "owner/public", repositoryId: 42, access: { type: "public" } }),
          x: 0, y: 0, preDeployCommand: null, startCommand: null, healthcheck: { type: "none" }, restartPolicy: "unless-stopped",
        });
        assert.strictEqual(publicService.data.identity.policy.autoDeploy, false);
        for (const policy of [{ autoDeploy: true }, { waitForCi: true }]) {
          const rejected = yield* editServiceMetadata(actor, {
            organizationSlug: "acme", environmentId: environmentRecord.id, serviceId: publicService.data.service.id,
            edit: { kind: "policy", policy },
          }).pipe(Effect.flip);
          assert.strictEqual(rejected._tag, "Validation");
        }
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

const hostedPolar: PolarService = {
  mode: "hosted",
  productId: "pro",
  listActiveSubscriptions: () => Effect.die("Custom domains read the cached billing row."),
  createCheckout: () => Effect.die("unused"),
};

it.live(
  "links custom domains on self-hosted or with an active hosted subscription",
  () =>
    Effect.gen(function* () {
      const container = yield* postgresTestContainer;
      yield* migrateTestDatabase(container.url);
      const config = AppConfig.layer.pipe(Layer.provide(ConfigProvider.layer(ConfigProvider.fromEnv({ env: {
        ...testConfigEnvironment(),
        DATABASE_URL: container.url.href,
      } }))));
      const layer = (polar: PolarService) => Layer.mergeAll(DatabaseLive.pipe(Layer.provide(config)),
        Layer.succeed(Polar, polar), SecretEncryptionLive.pipe(Layer.provide(config)));

      const seeded = yield* Effect.gen(function* () {
        const database = yield* Database;
        const [author] = yield* database.drizzle.insert(user)
          .values({ email: "domains@example.test", emailVerified: true, name: "Domains" }).returning({ id: user.id });
        const [org] = yield* database.drizzle.insert(organization).values({ name: "Acme", slug: "acme" }).returning({ id: organization.id });
        if (!author || !org) return yield* Effect.die("PostgreSQL did not return the seed rows.");
        yield* database.drizzle.insert(member).values({ userId: author.id, organizationId: org.id, role: "owner" });
        const [proj] = yield* database.drizzle.insert(project).values({ organizationId: org.id, name: "API", slug: "api" }).returning({ id: project.id });
        if (!proj) return yield* Effect.die("PostgreSQL did not return the project.");
        const [env] = yield* database.drizzle.insert(environment).values({ organizationId: org.id, projectId: proj.id,
          name: "Production", namespace: "api-production", intent: emptyEnvironmentIntent("api-production") }).returning({ id: environment.id });
        if (!env) return yield* Effect.die("PostgreSQL did not return the environment.");
        const actor = { userId: author.id };
        const created = yield* createService(actor, { organizationSlug: "acme", environmentId: env.id, name: "Web",
          source: createImageServiceSource({ image: "acme/web:latest" }), x: 0, y: 0,
          preDeployCommand: null, startCommand: null, healthcheck: { type: "none" }, restartPolicy: "unless-stopped" });
        return { actor, organizationId: org.id, environmentId: env.id, serviceId: created.data.service.id };
      }).pipe(Effect.provide(layer({ mode: "self_hosted" })));

      const linkRoute = (hostname: string) => Effect.gen(function* () {
        const { revision } = yield* loadEnvironmentDocument(seeded.environmentId);
        return yield* updateService(seeded.actor, { organizationSlug: "acme", environmentId: seeded.environmentId,
          serviceId: seeded.serviceId, revision, routes: [{ id: randomUUID(), hostname, targetPort: 3000 }] });
      });

      yield* linkRoute("self-hosted.example.com").pipe(Effect.provide(layer({ mode: "self_hosted" })));

      yield* Effect.gen(function* () {
        const refused = yield* linkRoute("unpaid.example.com").pipe(Effect.flip);
        assert.strictEqual(refused._tag, "Forbidden");
        assert.strictEqual(refused.message, "Custom domains require an active subscription.");
        const { intent } = yield* loadEnvironmentDocument(seeded.environmentId);
        assert.strictEqual(intent.services[0]?.config.routes[0]?.hostname, "self-hosted.example.com");

        const { revision } = yield* loadEnvironmentDocument(seeded.environmentId);
        yield* updateService(seeded.actor, { organizationSlug: "acme", environmentId: seeded.environmentId,
          serviceId: seeded.serviceId, revision, routes: [] });

        const database = yield* Database;
        const setSubscription = (hasActiveSubscription: boolean) => {
          const state = hasActiveSubscription
            ? { hasActiveSubscription, activeSubscriptionId: "sub-pro", currentPeriodEnd: new Date("2099-01-01T00:00:00Z") }
            : { hasActiveSubscription, activeSubscriptionId: null, currentPeriodEnd: null };
          return database.drizzle.insert(organizationBillingState).values({ organizationId: seeded.organizationId, ...state })
            .onConflictDoUpdate({ target: organizationBillingState.organizationId, set: state });
        };
        yield* setSubscription(false);
        assert.strictEqual((yield* linkRoute("lapsed.example.com").pipe(Effect.flip))._tag, "Forbidden");
        yield* setSubscription(true);
        yield* linkRoute("paid.example.com");
      }).pipe(Effect.provide(layer(hostedPolar)));
    }),
  60_000,
);
