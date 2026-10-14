import { realpathSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { createClientRpc } from "@tanstack/react-start/client-rpc";
import { runWithStartContext } from "@tanstack/start-storage-context";
import { ConfigProvider, Effect, Layer } from "effect";
import { Inngest } from "inngest";
import { Client } from "pg";
import { createServer } from "vite";
import { expect, it } from "vitest";
import { Polar } from "#/modules/billing/polar-provider.server";
import { InngestClient } from "#/modules/inngest/client";
import { Auth, AuthLive } from "#/server/auth.server";
import { AppConfig } from "#/server/config.server";
import { DatabaseLive } from "#/server/database.server";
import { testConfigEnvironment } from "#/test/config-environment";
import { postgresTestDatabase } from "#/test/postgres";

const configFile = fileURLToPath(new URL("../../../vite.config.ts", import.meta.url));
const CALLS_PER_USER = 20;

type Listed = { readonly members: ReadonlyArray<{ readonly email: string; readonly you: boolean }> };

it(
  "lists members as the user who asked, while another user lists theirs at the same time",
  () =>
    Effect.runPromise(Effect.scoped(Effect.gen(function* () {
      const testDatabase = yield* postgresTestDatabase;
      const configLayer = AppConfig.layer.pipe(Layer.provide(ConfigProvider.layer(ConfigProvider.fromEnv({
        env: { ...testConfigEnvironment(), NODE_ENV: "test", DATABASE_URL: testDatabase.url.href },
      }))));
      const authLayer = AuthLive.pipe(Layer.provide(Layer.mergeAll(
        configLayer,
        DatabaseLive.pipe(Layer.provide(configLayer)),
        Layer.succeed(Polar, { mode: "self_hosted" }),
        Layer.succeed(InngestClient, new Inngest({ id: "members-functions-test" })),
      )));
      const signUp = (email: string) => Effect.gen(function* () {
        const auth = yield* Auth;
        const response = yield* auth.handler(new Request("http://localhost:3000/api/auth/sign-up/email", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ email, name: email.split("@")[0], password: "correct-horse-battery-staple" }),
        }));
        const cookie = response.headers.get("set-cookie")?.split(";", 1)[0];
        if (cookie === undefined) return yield* Effect.die("Better Auth did not set a session cookie");
        return cookie;
      }).pipe(Effect.provide(authLayer));
      const users = [
        { email: "ada@example.test", cookie: yield* signUp("ada@example.test") },
        { email: "bob@example.test", cookie: yield* signUp("bob@example.test") },
      ];

      const slugs = yield* Effect.promise(async () => {
        const database = new Client({ connectionString: testDatabase.url.href });
        await database.connect();
        try {
          const rows = await database.query<{ email: string; slug: string }>(
            `select u.email, o.slug from member m join "user" u on u.id = m.user_id join organization o on o.id = m.organization_id`,
          );
          return new Map(rows.rows.map((row) => [row.email, row.slug]));
        } finally {
          await database.end();
        }
      });

      yield* Effect.promise(async () => {
        const previous = { DATABASE_URL: process.env["DATABASE_URL"], BETTER_AUTH_SECRET: process.env["BETTER_AUTH_SECRET"], TSS_SERVER_FN_BASE: process.env["TSS_SERVER_FN_BASE"] };
        process.env["DATABASE_URL"] = testDatabase.url.href;
        process.env["BETTER_AUTH_SECRET"] = "better-auth-secret";
        process.env["TSS_SERVER_FN_BASE"] = "/_serverFn/";
        const vite = await createServer({
          configFile,
          optimizeDeps: { noDiscovery: true },
          server: {
            host: "127.0.0.1",
            port: 0,
            strictPort: false,
            fs: { allow: [process.cwd(), realpathSync(fileURLToPath(new URL("../../../node_modules", import.meta.url)))] },
          },
        });
        try {
          await vite.listen();
          const origin = vite.resolvedUrls?.local[0];
          if (origin === undefined) throw new Error("Vite did not expose its URL.");
          const transformed = await vite.transformRequest("/src/modules/organization/members.functions.ts");
          const id = /listMembersServerFn\s*=[^\n]*?createClientRpc\("([^"]+)"\)/u.exec(transformed?.code ?? "")?.[1];
          if (id === undefined) throw new Error("TanStack did not compile listMembersServerFn.");
          const rpc = createClientRpc(id);
          const list = async (cookie: string, organizationSlug: string) => {
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
              method: "GET",
              data: { organizationSlug },
              headers: { cookie, "sec-fetch-site": "same-origin" },
              fetch: (input: RequestInfo | URL, init?: RequestInit) => fetch(new URL(input instanceof Request ? input.url : String(input), origin), init),
            }));
            if (frame.error) throw frame.error;
            return frame.result as Listed;
          };

          const calls = Array.from({ length: CALLS_PER_USER }, () => users).flat().map(async ({ email, cookie }) => {
            const result = await list(cookie, slugs.get(email) ?? "");
            return { asked: email, you: result.members.filter((row) => row.you).map((row) => row.email) };
          });
          const results = await Promise.allSettled(calls);
          expect(results.map((result) => result.status === "fulfilled" ? result.value.you : result.reason))
            .toEqual(Array.from({ length: CALLS_PER_USER }, () => users.map(({ email }) => [email])).flat());
        } finally {
          await vite.close();
          for (const [key, value] of Object.entries(previous)) {
            if (value === undefined) delete process.env[key];
            else process.env[key] = value;
          }
        }
      });
    }))),
  120_000,
);
