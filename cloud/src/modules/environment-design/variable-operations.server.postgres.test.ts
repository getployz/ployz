import { assert, it } from "@effect/vitest";
import { sql } from "drizzle-orm";
import { ConfigProvider, Effect, Layer } from "effect";
import {
  environment,
  environmentVariableGroup,
  member,
  organization,
  project,
  serviceVariableGroupAttachment,
  user,
  variableGroupLineage,
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
  "authorizes variable writes, redacts secrets, and reconciles every transaction receipt",
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
                ELECTRIC_URL: "http://localhost:30000",
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
        const lineages = yield* database.drizzle
          .insert(variableGroupLineage)
          .values({
            projectId: projectRecord.id,
            canonicalName: "Shared",
            canonicalSlug: "shared-lineage",
          })
          .returning({ id: variableGroupLineage.id });
        const lineage = lineages[0];
        if (lineage === undefined) {
          return yield* Effect.die("PostgreSQL did not return the group lineage.");
        }
        const groups = yield* database.drizzle
          .insert(environmentVariableGroup)
          .values({
            organizationId: organizationRecord.id,
            projectId: projectRecord.id,
            environmentId: environmentRecord.id,
            lineageId: lineage.id,
            name: "Shared",
            slug: "shared",
          })
          .returning({ id: environmentVariableGroup.id });
        const group = groups[0];
        if (group === undefined) {
          return yield* Effect.die("PostgreSQL did not return the variable group.");
        }

        const plain = yield* createServiceVariable(actor, {
          organizationSlug: "acme",
          environmentId: environmentRecord.id,
          serviceId,
          key: "HOST",
          description: null,
          exported: false,
          value: { type: "plain", value: "localhost" },
        });
        const plainRows = yield* database.drizzle.execute<{ txid: string }>(
          sql`
            select xmin::text as txid from variable where id = ${plain.data.id}
            union all
            select xmin::text as txid from config_key where id = ${plain.data.configKeyId}
            union all
            select xmin::text as txid from config_value
            where config_key_id = ${plain.data.configKeyId}
              and environment_id = ${environmentRecord.id}
          `,
          "objects",
        );
        assert.deepStrictEqual(
          plainRows.map((row) => Number(row.txid)),
          [plain.txid, plain.txid, plain.txid],
        );

        const sealed = yield* createVariableGroupVariable(actor, {
          organizationSlug: "acme",
          environmentId: environmentRecord.id,
          variableGroupId: group.id,
          key: "TOKEN",
          description: "private",
          exported: true,
          value: { type: "sealed", value: "never-return-this" },
        });
        assert.strictEqual(sealed.data.value.type, "sealed");
        assert.strictEqual("value" in sealed.data.value, false);
        const secretRows = yield* database.drizzle.execute<{ txid: string }>(
          sql`
            select xmin::text as txid from variable where id = ${sealed.data.id}
            union all
            select xmin::text as txid from variable_secret
            where variable_id = ${sealed.data.id}
            union all
            select xmin::text as txid from config_key where id = ${sealed.data.configKeyId}
            union all
            select xmin::text as txid from config_value
            where config_key_id = ${sealed.data.configKeyId}
              and environment_id = ${environmentRecord.id}
          `,
          "objects",
        );
        assert.deepStrictEqual(
          secretRows.map((row) => Number(row.txid)),
          [sealed.txid, sealed.txid, sealed.txid, sealed.txid],
        );

        const invalidTransition = yield* Effect.flip(
          updateVariableGroupVariable(actor, {
            organizationSlug: "acme",
            environmentId: environmentRecord.id,
            variableGroupId: group.id,
            variableId: sealed.data.id,
            key: "TOKEN",
            description: "private",
            exported: true,
            value: { type: "plain", value: "exposed" },
          }),
        );
        assert.strictEqual(invalidTransition._tag, "Validation");

        const attachment = yield* attachServiceVariableGroup(actor, {
          organizationSlug: "acme",
          environmentId: environmentRecord.id,
          serviceId,
          variableGroupId: group.id,
        });
        const attachmentRows = yield* database.drizzle.execute<{ txid: string }>(
          sql`select xmin::text as txid from service_variable_group_attachment
              where service_id = ${serviceId} and variable_group_id = ${group.id}`,
          "objects",
        );
        assert.deepStrictEqual(
          attachmentRows.map((row) => Number(row.txid)),
          [attachment.txid],
        );
        const bulk = yield* bulkUpdateServiceVariables(actor, {
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
              variableId: plain.data.id,
              key: "HOST",
              value: { type: "plain", value: "127.0.0.1" },
            },
          ],
          deletes: [],
        });
        const bulkRows = yield* database.drizzle.execute<{ txid: string }>(
          sql`select xmin::text as txid from variable
              where service_id = ${serviceId} order by key`,
          "objects",
        );
        assert.deepStrictEqual(
          bulkRows.map((row) => Number(row.txid)),
          [bulk.txid, bulk.txid],
        );

        const storedAttachment = yield* database.drizzle
          .select({ serviceId: serviceVariableGroupAttachment.serviceId })
          .from(serviceVariableGroupAttachment);
        assert.deepStrictEqual(storedAttachment, [{ serviceId }]);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);
