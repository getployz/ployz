import { environmentSavedStateSnapshot } from "#/modules/deployments/tables";
import { collectionReadInput } from "./read.contract";
import { assert, it } from "@effect/vitest";
import { ConfigProvider, Effect, Layer, Schema } from "effect";
import { Inngest } from "inngest";
import { readCollection, CollectionReadInvalid } from "./read.server";
import { githubRepositoryCache } from "#/modules/github/tables";
import { project, environment } from "#/modules/project/tables";
import { member } from "#/modules/identity/tables";
import { organization } from "#/modules/organization/tables";
import { Polar } from "#/modules/billing/polar-provider.server";
import { InngestClient } from "#/modules/inngest/client";
import { Auth, AuthLive } from "#/server/auth.server";
import { AppConfig } from "#/server/config.server";
import { Database, DatabaseLive } from "#/server/database.server";
import { publicErrorResponse } from "#/server/public-error";
import {
  migrateTestDatabase,
  postgresTestContainer,
} from "#/test/postgres";

const privateHeaders = { "cache-control": "private, no-store" } as const;

function execute(request: Request, input: { table: string; userId: string; organizationSlug?: string; sql?: string }) {
  return Effect.gen(function* () {
    const auth = yield* Auth;
    const actor = yield* auth.resolveActor(request.headers);
    const data = yield* Schema.decodeUnknownEffect(collectionReadInput)(input, { onExcessProperty: "error" })
      .pipe(Effect.mapError(() => new CollectionReadInvalid({ message: "Invalid collection read." })));
    const rows = yield* readCollection(actor, data);
    return Response.json(rows, { headers: privateHeaders });
  }).pipe(Effect.catch((cause) => Effect.succeed(publicErrorResponse(cause, {
    headers: privateHeaders,
  }))));
}

it.live(
  "authenticates and isolates allowlisted collection snapshots",
  () =>
    Effect.gen(function* () {
      const container = yield* postgresTestContainer;
      yield* migrateTestDatabase(container.url);
      const provider = ConfigProvider.fromEnv({
        env: {
          NODE_ENV: "test",
          DATABASE_URL: container.url.href,
          APP_URL: "http://localhost:3000",
          BETTER_AUTH_SECRET: "better-auth-secret",
          GITHUB_CLIENT_ID: "github-client-id",
          GITHUB_CLIENT_SECRET: "github-client-secret",
          APP_ENCRYPTION_SECRET:
            "app-encryption-secret-at-least-32-characters",
        },
      });
      const configLayer = AppConfig.layer.pipe(
        Layer.provide(ConfigProvider.layer(provider)),
      );
      const databaseLayer = DatabaseLive.pipe(Layer.provide(configLayer));
      const layer = AuthLive.pipe(
        Layer.provideMerge(Layer.mergeAll(
          configLayer,
          databaseLayer,
          Layer.succeed(Polar, { mode: "self_hosted" }),
          Layer.succeed(
            InngestClient,
            new Inngest({ id: "table-sync-contract" }),
          ),
        )),
      );
      yield* Effect.gen(function* () {
        const anonymous = yield* execute(new Request("http://app.test"), {
          table: "github_repository_cache", userId: "nobody",
        });
        assert.strictEqual(anonymous.status, 401);
        const auth = yield* Auth;
        const signUp = yield* auth.handler(
          new Request("http://localhost:3000/api/auth/sign-up/email", {
            method: "POST",
            headers: { "content-type": "application/json" },
            body: JSON.stringify({
              email: "reader@example.test",
              name: "Reader",
              password: "correct-horse-battery-staple",
            }),
          }),
        );
        const cookie = signUp.headers.get("set-cookie")?.split(";", 1)[0];
        if (cookie === undefined) {
          return yield* Effect.die("Better Auth did not set a session cookie");
        }
        const session = yield* auth.getSession(new Headers({ cookie }));
        if (session === null) {
          return yield* Effect.die("Better Auth did not resolve the session");
        }
        const database = yield* Database;
        const organizationId = crypto.randomUUID();
        yield* database.drizzle.insert(organization).values({
          id: organizationId,
          name: "Acme",
          slug: "acme-table-sync",
        });
        yield* database.drizzle.insert(member).values({
          userId: session.user.id,
          organizationId,
          role: "owner",
        });
        const headers = { cookie };
        const request = new Request("http://app.test", { headers });
        const userId = session.user.id;
        const base = { table: "github_repository_cache", userId };
        for (const input of [
          { ...base, table: "session" },
          { ...base, table: "organization_machine" },
          { ...base, table: "organization_pairing" },
          { ...base, table: "toString" },
          { ...base, sql: "select * from session" },
        ]) {
          assert.strictEqual((yield* execute(request, input)).status, 422);
        }
        assert.strictEqual((yield* execute(request, { ...base, userId: "other" })).status, 404);
        assert.strictEqual((yield* execute(request, { table: "project", userId })).status, 422);
        assert.strictEqual((yield* execute(request, {
          table: "project", userId, organizationSlug: "other",
        })).status, 404);
        const otherSignup = yield* auth.handler(new Request("http://localhost:3000/api/auth/sign-up/email", {
          method: "POST", headers: { "content-type": "application/json" },
          body: JSON.stringify({ email: "other@example.test", name: "Other", password: "correct-horse-battery-staple" }),
        }));
        const otherCookie = otherSignup.headers.get("set-cookie")?.split(";", 1)[0];
        const otherSession = yield* auth.getSession(new Headers({ cookie: otherCookie ?? "" }));
        if (!otherSession) return yield* Effect.die("Missing second session");
        const row = {
          installationId: 42, repositoryId: 9007199254740990, name: "repo", fullName: "acme/repo",
          defaultBranch: "main", private: true, htmlUrl: "https://github.com/acme/repo",
          repoUpdatedAt: new Date("2026-01-01T00:00:00Z"), syncedAt: new Date("2026-01-02T00:00:00Z"),
        };
        yield* database.drizzle.insert(githubRepositoryCache).values([
          { ...row, userId }, { ...row, userId: otherSession.user.id, name: "private-other" },
        ]);
        const response = yield* execute(request, base);
        assert.strictEqual(response.status, 200);
        assert.strictEqual(response.headers.get("cache-control"), "private, no-store");
        assert.deepStrictEqual(yield* Effect.promise(() => response.json()), [{
          ...row, userId, repoUpdatedAt: "2026-01-01T00:00:00.000Z", syncedAt: "2026-01-02T00:00:00.000Z",
        }]);
        const otherResponse = yield* execute(new Request("http://app.test", { headers: { cookie: otherCookie ?? "" } }), {
          ...base, userId: otherSession.user.id,
        });
        assert.deepStrictEqual((yield* Effect.promise(() => otherResponse.json())).map((row: { name: string }) => row.name), ["private-other"]);
        const otherOrganizationId = crypto.randomUUID();
        yield* database.drizzle.insert(organization).values({ id: otherOrganizationId, name: "Other", slug: "other-org" });
        const projects = yield* database.drizzle.insert(project).values([
          { organizationId, name: "Visible", slug: "visible" },
          { organizationId: otherOrganizationId, name: "Private", slug: "private" },
        ]).returning();
        for (const row of projects) {
          yield* database.drizzle.insert(environment).values({ organizationId: row.organizationId, projectId: row.id,
            name: row.name, namespace: "production",
            intent: { version: 1, environmentSlug: "production", services: [], volumes: [], variableGroups: [] },
          });
        }
        const environments = yield* database.drizzle.select().from(environment);
        const savedRows = yield* database.drizzle.insert(environmentSavedStateSnapshot).values(environments.map((row) => ({
          organizationId: row.organizationId, environmentId: row.id, actorId: userId,
          message: "Private revision message", intent: { private: "snapshot-content" },
          volumeDeletionAuthorizations: [{ private: "admission-evidence" }],
        }))).returning();
        const snapshotResponse = yield* execute(request, { table: "environment_saved_state_snapshot", userId,
          organizationSlug: "acme-table-sync" });
        assert.strictEqual(snapshotResponse.status, 200);
        assert.deepStrictEqual(yield* Effect.promise(() => snapshotResponse.json()),
          savedRows.filter((row) => row.organizationId === organizationId).map((row) => ({
            id: row.id, organizationId, environmentId: row.environmentId,
          })));
        for (const table of ["project", "environment"]) {
          const response = yield* execute(request, { table, userId, organizationSlug: "acme-table-sync" });
          const rows = yield* Effect.promise(() => response.json());
          assert.strictEqual(response.status, 200);
          assert.strictEqual(rows.length, 1);
          assert.strictEqual(rows[0].organizationId, organizationId);
          assert.strictEqual(rows[0].name, "Visible");
          assert.strictEqual(yield* Schema.decodeUnknownEffect(Schema.String)(rows[0].createdAt), rows[0].createdAt);
          assert.strictEqual((yield* execute(request, { table, userId, organizationSlug: "other-org" })).status, 404);
          assert.strictEqual((yield* execute(new Request("http://app.test", { headers: { cookie: otherCookie ?? "" } }),
            { table, userId: otherSession.user.id, organizationSlug: "acme-table-sync" })).status, 404);
        }
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);
