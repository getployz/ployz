import { assert, it } from "@effect/vitest";
import { ConfigProvider, Effect, Layer } from "effect";
import { Inngest } from "inngest";
import { vi } from "vitest";
import {
  handleTableSyncRequest,
} from "#/electric/table-sync-request.server";
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

function execute(request: Request, table: string) {
  return handleTableSyncRequest(request, table).pipe(
    Effect.match({
      onFailure: (cause) => publicErrorResponse(cause, {
        headers: privateHeaders,
      }),
      onSuccess: (response) => response,
    }),
  );
}

it.live(
  "authenticates, scopes, and proxies Electric table sync requests",
  () =>
    Effect.gen(function* () {
      const container = yield* postgresTestContainer;
      yield* migrateTestDatabase(container.url);
      const provider = ConfigProvider.fromEnv({
        env: {
          NODE_ENV: "test",
          DATABASE_URL: container.url.href,
          ELECTRIC_URL: "https://electric.internal:3000",
          ELECTRIC_SECRET: "server-secret",
          APP_URL: "http://localhost:3000",
          BETTER_AUTH_SECRET: "better-auth-secret",
          GITHUB_CLIENT_ID: "github-client-id",
          GITHUB_CLIENT_SECRET: "github-client-secret",
          PLOYZ_RELAY_URL: "https://relay.example.test",
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
      const requests: Array<{ readonly url: URL; readonly init?: RequestInit }> =
        [];
      const fetchSpy = yield* Effect.acquireRelease(
        Effect.sync(() =>
          vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
            requests.push({ url: new URL(String(input)), init });
            return new Response("[]", {
              headers: {
                "content-type": "application/json",
                "content-encoding": "gzip",
                "content-length": "2",
                "electric-offset": "0_0",
              },
            });
          })
        ),
        (spy) => Effect.sync(() => spy.mockRestore()),
      );

      yield* Effect.gen(function* () {
        const unknown = yield* execute(
          new Request("http://app.test/api/shapes/not_a_table"),
          "not_a_table",
        );
        assert.strictEqual(unknown.status, 404);

        const anonymous = yield* execute(
          new Request(
            "http://app.test/api/shapes/service?organizationSlug=acme",
          ),
          "service",
        );
        assert.strictEqual(anonymous.status, 401);
        assert.strictEqual(requests.length, 0);

        const auth = yield* Auth;
        const signUp = yield* auth.handler(
          new Request("http://localhost:3000/api/auth/sign-up/email", {
            method: "POST",
            headers: { "content-type": "application/json" },
            body: JSON.stringify({
              email: "electric@example.test",
              name: "Electric",
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

        const missingScope = yield* execute(
          new Request("http://app.test/api/shapes/service", { headers }),
          "service",
        );
        assert.strictEqual(missingScope.status, 422);
        const inaccessibleScope = yield* execute(
          new Request(
            "http://app.test/api/shapes/service?organizationSlug=other",
            { headers },
          ),
          "service",
        );
        assert.strictEqual(inaccessibleScope.status, 404);

        fetchSpy.mockRejectedValueOnce(new Error("secret upstream token"));
        const failed = yield* execute(
          new Request(
            "http://app.test/api/shapes/github_repository_cache",
            { headers },
          ),
          "github_repository_cache",
        );
        assert.strictEqual(failed.status, 500);
        assert.deepStrictEqual(yield* Effect.promise(() => failed.json()), {
          _tag: "PublicError",
          code: "INTERNAL",
          message: "The request could not be completed.",
        });

        const service = yield* execute(
          new Request(
            "http://app.test/api/shapes/service?organizationSlug=acme-table-sync&table=session&columns=token&where=true&offset=-1",
            { headers },
          ),
          "service",
        );
        assert.strictEqual(service.status, 200);
        const serviceRequest = requests.at(-1);
        if (serviceRequest === undefined) {
          return yield* Effect.die("Electric did not receive the request");
        }
        assert.strictEqual(serviceRequest.url.pathname, "/v1/shape");
        assert.strictEqual(serviceRequest.url.searchParams.get("table"), "service");
        assert.strictEqual(
          serviceRequest.url.searchParams.get("where"),
          "organization_id = $1",
        );
        assert.strictEqual(
          serviceRequest.url.searchParams.get("params[1]"),
          organizationId,
        );
        assert.strictEqual(serviceRequest.url.searchParams.get("offset"), "-1");
        assert.strictEqual(
          serviceRequest.url.searchParams.get("secret"),
          "server-secret",
        );
        assert.strictEqual(serviceRequest.url.searchParams.has("columns"), false);
        assert.strictEqual(service.headers.get("content-encoding"), null);
        assert.strictEqual(service.headers.get("content-length"), null);
        assert.strictEqual(
          service.headers.get("cache-control"),
          "private, no-store",
        );

        yield* execute(
          new Request(
            "http://app.test/api/shapes/service?organizationSlug=acme-table-sync",
            {
              method: "POST",
              headers: { ...headers, "content-type": "application/json" },
              body: '{"live":true}',
            },
          ),
          "service",
        );
        const posted = requests.at(-1)?.init;
        assert.strictEqual(posted?.method, "POST");
        assert.strictEqual(
          new Headers(posted?.headers).get("content-type"),
          "application/json",
        );
        assert.strictEqual(
          new TextDecoder().decode(posted?.body as ArrayBuffer),
          '{"live":true}',
        );

        yield* execute(
          new Request(
            "http://app.test/api/shapes/github_repository_cache",
            { headers },
          ),
          "github_repository_cache",
        );
        const userRequest = requests.at(-1);
        assert.strictEqual(
          userRequest?.url.searchParams.get("where"),
          "user_id = $1",
        );
        assert.strictEqual(
          userRequest?.url.searchParams.get("params[1]"),
          session.user.id,
        );

        yield* execute(
          new Request(
            "http://app.test/api/shapes/environment_saved_state_snapshot?organizationSlug=acme-table-sync&columns=intent,volume_deletion_authorizations",
            { headers },
          ),
          "environment_saved_state_snapshot",
        );
        const snapshotRequest = requests.at(-1);
        assert.strictEqual(
          snapshotRequest?.url.searchParams.get("columns"),
          '"id","organization_id","environment_id"',
        );
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);
