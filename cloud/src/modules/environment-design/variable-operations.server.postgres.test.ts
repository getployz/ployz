import { loadEnvironmentDocument } from "./working-state-repository.server";
import { emptyEnvironmentIntent } from "./saved-intent";
import { createVariableGroupResource } from "./resource-operations.server";
import { assert, it } from "@effect/vitest";
import { sql } from "drizzle-orm";
import { ConfigProvider, Effect, Layer } from "effect";
import {
  environment,
  member,
  organization,
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
import { createService } from "./service-operations.server";
import { createImageServiceSource } from "./services";
import {
  attachServiceVariableGroup,
  bulkUpdateServiceVariables,
  createServiceVariable,
  createVariableGroupVariable,
  updateVariableGroupVariable,
} from "./variable-operations.server";

it.live(
  "authorizes variable writes, redacts secrets, and commits related rows atomically",
  () =>
    Effect.gen(function* () {
      const container = yield* postgresTestContainer;
      yield* migrateTestDatabase(container.url);
      const config = AppConfig.layer.pipe(
        Layer.provide(
          ConfigProvider.layer(
            ConfigProvider.fromEnv({
              env: {
                DATABASE_URL: container.url.href,
                APP_URL: "http://localhost:3000",
                BETTER_AUTH_SECRET: "better-auth-secret",
                GITHUB_CLIENT_ID: "github-client-id",
                GITHUB_CLIENT_SECRET: "github-client-secret",
                PLOYZ_RELAY_URL: "https://relay.example.test",
                APP_ENCRYPTION_SECRET:
                  "app-encryption-secret-at-least-32-characters",
              },
            }),
          ),
        ),
      );
      const layer = Layer.merge(
        DatabaseLive.pipe(Layer.provide(config)),
        SecretEncryptionLive.pipe(Layer.provide(config)),
      );

      yield* Effect.gen(function* () {
        const database = yield* Database;
        const users = yield* database.drizzle
          .insert(user)
          .values([
            { email: "variables@example.test", emailVerified: true, name: "Variables" },
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
        const createdService = yield* createService(actor, {
          organizationSlug: "acme",
          environmentId: environmentRecord.id,
          name: "Registry",
          source: createImageServiceSource({ image: "acme/api:latest" }),
          x: 0,
          y: 0,
          preDeployCommand: null,
          startCommand: null,
          healthcheck: { type: "none" },
          restartPolicy: "unless-stopped",
        });
        const serviceId = createdService.data.service.id;
        const createdGroup = yield* createVariableGroupResource(actor, { organizationSlug: "acme", environmentId: environmentRecord.id, name: "Shared", x: 0, y: 0 });
        const group = createdGroup.data.variableGroup;

        const plain = yield* createServiceVariable(actor, {
          revision: (yield* loadEnvironmentDocument(environmentRecord.id)).revision,
          organizationSlug: "acme",
          environmentId: environmentRecord.id,
          serviceId,
          key: "HOST",
          description: null,
          exported: false,
          value: { type: "plain", value: "localhost" },
        });
        const plainId = plain.data.intent.services[0]?.variables[0]?.id;
        if (!plainId) return yield* Effect.die("Plain variable missing.");
        const plainRows = yield* database.drizzle.execute<{ txid: string }>(
          sql`select xmin::text as txid from variable where id = ${plainId}
              union all select xmin::text as txid from environment where id = ${environmentRecord.id}`, "objects");
        assert.strictEqual(plainRows.length, 2);
        assert.strictEqual(new Set(plainRows.map((row) => row.txid)).size, 1);

        const sealed = yield* createVariableGroupVariable(actor, {
          revision: (yield* loadEnvironmentDocument(environmentRecord.id)).revision,
          organizationSlug: "acme",
          environmentId: environmentRecord.id,
          variableGroupId: group.id,
          key: "TOKEN",
          description: "private",
          exported: true,
          value: { type: "sealed", value: "never-return-this" },
        });
        const sealedVariable = sealed.data.intent.variableGroups[0]?.variables[0];
        if (!sealedVariable) return yield* Effect.die("Secret variable missing.");
        assert.strictEqual(sealedVariable.value.kind, "secret");
        assert.ok(!JSON.stringify(sealed.data).includes("never-return-this"));
        assert.ok(!JSON.stringify(sealed.data).includes("ciphertext"));
        const secretRows = yield* database.drizzle.execute<{ txid: string }>(
          sql`select xmin::text as txid from variable where id = ${sealedVariable.id}
              union all select xmin::text as txid from variable_secret where variable_id = ${sealedVariable.id}
              union all select xmin::text as txid from environment where id = ${environmentRecord.id}`, "objects");
        assert.strictEqual(secretRows.length, 3);
        assert.strictEqual(new Set(secretRows.map((row) => row.txid)).size, 1);

        const invalidTransition = yield* Effect.flip(
          updateVariableGroupVariable(actor, {
          revision: (yield* loadEnvironmentDocument(environmentRecord.id)).revision,
            organizationSlug: "acme",
            environmentId: environmentRecord.id,
            variableGroupId: group.id,
            variableId: sealedVariable.id,
            key: "TOKEN",
            description: "private",
            exported: true,
            value: { type: "plain", value: "exposed" },
          }),
        );
        assert.strictEqual(invalidTransition._tag, "Validation");

        const attachment = yield* attachServiceVariableGroup(actor, {
          revision: (yield* loadEnvironmentDocument(environmentRecord.id)).revision,
          organizationSlug: "acme",
          environmentId: environmentRecord.id,
          serviceId,
          variableGroupId: group.id,
        });
        assert.strictEqual(attachment.data.intent.services[0]?.variableGroupAttachments[0]?.variableGroupId, group.id);
        const bulk = yield* bulkUpdateServiceVariables(actor, {
          revision: (yield* loadEnvironmentDocument(environmentRecord.id)).revision,
          organizationSlug: "acme",
          environmentId: environmentRecord.id,
          serviceId,
          creates: [
            {
              key: "PORT",
              description: null,
              exported: false,
              value: { type: "plain", value: "3000" },
            },
          ],
          updates: [
            {
              variableId: plainId,
              key: "HOST",
              value: { type: "plain", value: "127.0.0.1" },
            },
          ],
          deletes: [],
        });
        assert.deepStrictEqual(bulk.data.intent.services[0]?.variables.map((variable) => [variable.key, variable.value]).sort((a, b) => String(a[0]).localeCompare(String(b[0]))), [
          ["HOST", { kind: "literal", value: "127.0.0.1" }], ["PORT", { kind: "literal", value: "3000" }],
        ]);
        assert.strictEqual(
          (yield* loadEnvironmentDocument(environmentRecord.id)).revision,
          bulk.data.revision,
        );
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);
