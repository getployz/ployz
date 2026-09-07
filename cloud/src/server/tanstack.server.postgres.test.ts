import { realpathSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { createClientRpc } from "@tanstack/react-start/client-rpc";
import { runWithStartContext } from "@tanstack/start-storage-context";
import { ConfigProvider, Effect, Layer, Schema } from "effect";
import { Inngest } from "inngest";
import { createServer } from "vite";
import { expect, it } from "vitest";
import { Auth, AuthLive } from "#/server/auth.server";
import { AppConfig } from "#/server/config.server";
import { DatabaseLive } from "#/server/database.server";
import { Polar } from "#/modules/billing/polar-provider.server";
import { InngestClient } from "#/modules/inngest/client";
import { migrateTestDatabase, postgresTestContainer } from "#/test/postgres";

const configFile = fileURLToPath(new URL("../../vite.config.ts", import.meta.url));
const BoundaryResult = Schema.Union([
  Schema.Struct({ userId: Schema.String }),
  Schema.Struct({ disposed: Schema.Literal(true) }),
]);
type BoundaryResult = typeof BoundaryResult.Type;
const decodeBoundaryResult = Schema.decodeUnknownSync(BoundaryResult);
const GithubInstallResult = Schema.Struct({
  url: Schema.NullOr(Schema.String),
  configured: Schema.Boolean,
});
const decodeGithubInstallResult = Schema.decodeUnknownSync(GithubInstallResult);

type BoundaryData = {
  readonly action: "actor" | "fail" | "dispose";
  readonly unexpected?: boolean;
};

it(
  "round-trips strict validation, Actor, redacted errors, and cancellation through TanStack",
  () =>
    Effect.runPromise(Effect.scoped(Effect.gen(function* () {
      const container = yield* postgresTestContainer;
      yield* migrateTestDatabase(container.url);
      const provider = ConfigProvider.fromEnv({
        env: {
          NODE_ENV: "test",
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
      });
      const configLayer = AppConfig.layer.pipe(
        Layer.provide(ConfigProvider.layer(provider)),
      );
      const databaseLayer = DatabaseLive.pipe(Layer.provide(configLayer));
      const authLayer = AuthLive.pipe(
        Layer.provide(Layer.mergeAll(
          configLayer,
          databaseLayer,
          Layer.succeed(Polar, { mode: "self_hosted" }),
          Layer.succeed(
            InngestClient,
            new Inngest({ id: "tanstack-boundary-test" }),
          ),
        )),
      );
      const cookie = yield* Effect.gen(function* () {
        const auth = yield* Auth;
        const response = yield* auth.handler(
            new Request("http://localhost:3000/api/auth/sign-up/email", {
              method: "POST",
              headers: { "content-type": "application/json" },
              body: JSON.stringify({
                email: "boundary@example.test",
                name: "Boundary",
                password: "correct-horse-battery-staple",
              }),
            }),
          );
        const value = response.headers.get("set-cookie")?.split(";", 1)[0];
        if (value === undefined) {
          return yield* Effect.die("Better Auth did not set a session cookie");
        }
        return value;
      }).pipe(Effect.provide(authLayer));

      yield* Effect.promise(async () => {
        const previousDatabaseUrl = process.env["DATABASE_URL"];
        const previousAuthSecret = process.env["BETTER_AUTH_SECRET"];
        const previousServerFnBase = process.env["TSS_SERVER_FN_BASE"];
        process.env["DATABASE_URL"] = container.url.href;
        process.env["BETTER_AUTH_SECRET"] = "better-auth-secret";
        process.env["TSS_SERVER_FN_BASE"] = "/_serverFn/";

        const vite = await createServer({
          configFile,
          optimizeDeps: { noDiscovery: true },
          server: {
            host: "127.0.0.1",
            port: 0,
            strictPort: false,
            fs: {
              allow: [
                process.cwd(),
                realpathSync(fileURLToPath(new URL("../../node_modules", import.meta.url))),
              ],
            },
          },
        });
        try {
          await vite.listen();
          const origin = vite.resolvedUrls?.local[0];
          if (origin === undefined) throw new Error("Vite did not expose its URL.");
          const transformed = await vite.transformRequest(
            "/src/test/fixtures/effect-boundary.functions.ts",
          );
          const serverFnId = /createClientRpc\("([^"]+)"\)/u.exec(
            transformed?.code ?? "",
          )?.[1];
          if (serverFnId === undefined) {
            throw new Error("TanStack did not compile the boundary server function.");
          }
          const githubTransformed = await vite.transformRequest(
            "/src/modules/github/github.functions.ts",
          );
          const githubInstallServerFnId = /createClientRpc\("([^"]+)"\)/u.exec(
            githubTransformed?.code ?? "",
          )?.[1];
          if (githubInstallServerFnId === undefined) {
            throw new Error("TanStack did not compile the GitHub install server function.");
          }
          const baseFetch: typeof fetch = (input, init) => {
            const url = input instanceof Request ? input.url : String(input);
            return fetch(new URL(url, origin), init);
          };
          const rpc = createClientRpc(serverFnId);
          const githubInstallRpc = createClientRpc(githubInstallServerFnId);
          const invoke = async (data: BoundaryData, signal?: AbortSignal) => {
            const frame = await runWithStartContext({
              getRouter: () => {
                throw new Error("The client RPC test does not use a router.");
              },
              request: new Request(origin),
              startOptions: {},
              contextAfterGlobalMiddlewares: {},
              executedRequestMiddlewares: new Set(),
              handlerType: "serverFn",
            }, () => rpc({
              method: "POST",
              data,
              signal,
              headers: { cookie, "sec-fetch-site": "same-origin" },
              fetch: baseFetch,
            }));
            if (frame.error) throw frame.error;
            return decodeBoundaryResult(frame.result, {
              onExcessProperty: "error",
            });
          };
          const invokeGithubInstall = async (authenticated: boolean) => {
            const frame = await runWithStartContext({
              getRouter: () => {
                throw new Error("The client RPC test does not use a router.");
              },
              request: new Request(origin),
              startOptions: {},
              contextAfterGlobalMiddlewares: {},
              executedRequestMiddlewares: new Set(),
              handlerType: "serverFn",
            }, () => githubInstallRpc({
              method: "GET",
              data: undefined,
              headers: authenticated
                ? { cookie, "sec-fetch-site": "same-origin" }
                : { "sec-fetch-site": "same-origin" },
              fetch: baseFetch,
            }));
            if (frame.error) throw frame.error;
            return decodeGithubInstallResult(frame.result, {
              onExcessProperty: "error",
            });
          };

          try {
            await expect(
              invoke({ action: "actor" }),
            ).resolves.toEqual({ userId: expect.any(String) });

            await expect(
              invoke({ action: "actor", unexpected: true }),
            ).rejects.toEqual({
              _tag: "PublicError",
              code: "VALIDATION_FAILED",
              message: "The request is invalid.",
            });

            try {
              await invoke({ action: "fail" });
              expect.unreachable("Expected the Effect program to fail");
            } catch (cause) {
              expect(cause).toEqual({
                _tag: "PublicError",
                code: "INTERNAL",
                message: "The request could not be completed.",
              });
              expect(cause).not.toBeInstanceOf(Error);
              expect(JSON.stringify(cause)).not.toMatch(
                /application-secret|provider-token|db\.internal|stack|cause/u,
              );
            }

            await expect(invokeGithubInstall(false)).rejects.toEqual({
              _tag: "PublicError",
              code: "UNAUTHORIZED",
              message: "Authentication is required.",
            });
            await expect(invokeGithubInstall(true)).resolves.toEqual({
              url: "https://github.com/apps/test-app/installations/new",
              configured: true,
            });
          } finally {
            await expect(invoke({ action: "dispose" })).resolves.toEqual({
              disposed: true,
            });
          }
        } finally {
          await vite.close();
          if (previousDatabaseUrl === undefined) {
            delete process.env["DATABASE_URL"];
          } else {
            process.env["DATABASE_URL"] = previousDatabaseUrl;
          }
          if (previousAuthSecret === undefined) {
            delete process.env["BETTER_AUTH_SECRET"];
          } else {
            process.env["BETTER_AUTH_SECRET"] = previousAuthSecret;
          }
          if (previousServerFnBase === undefined) {
            delete process.env["TSS_SERVER_FN_BASE"];
          } else {
            process.env["TSS_SERVER_FN_BASE"] = previousServerFnBase;
          }
        }
      });
    }))),
  120_000,
);
