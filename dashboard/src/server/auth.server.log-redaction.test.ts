import { testConfigEnvironment } from "#/test/config-environment";
import { ConfigProvider, Effect, Layer } from "effect";
import { Inngest } from "inngest";
import { formatWithOptions } from "node:util";
import { afterEach, expect, it, vi } from "vitest";
import { Polar } from "#/modules/billing/polar-provider.server";
import { InngestClient } from "#/modules/inngest/client";
import { Auth, AuthLive } from "#/server/auth.server";
import { AppConfig } from "#/server/config.server";
import { DatabaseLive } from "#/server/database.server";
import { RedactedLogsLive } from "#/server/log-redaction";
import { publicErrorResponse } from "#/server/public-error";

const FAKE_SESSION_TOKEN = "fake-session-token-0123456789abc";
const FAKE_EMAIL = "fake-user@example.test";

// A closed port: every query fails as it would while Postgres is down.
const unreachableAuth = (() => {
  const provider = ConfigProvider.fromEnv({
    env: {
      ...testConfigEnvironment(),
      NODE_ENV: "test",
      DATABASE_URL: "postgres://ployz:ployz@127.0.0.1:9/ployz",
    },
  });
  const configLayer = AppConfig.layer.pipe(Layer.provide(ConfigProvider.layer(provider)));
  return AuthLive.pipe(
    Layer.provideMerge(Layer.mergeAll(
      configLayer,
      DatabaseLive.pipe(Layer.provide(configLayer)),
      Layer.succeed(Polar, { mode: "self_hosted" }),
      Layer.succeed(InngestClient, new Inngest({ id: "auth-log-redaction-test" })),
    )),
    Layer.provide(RedactedLogsLive),
  );
})();

/** Everything written to the console or stdio while `program` runs. */
async function captureOutput(program: Effect.Effect<void>) {
  const lines: string[] = [];
  const record = (...args: unknown[]) => {
    lines.push(formatWithOptions({ depth: Infinity }, ...args));
  };
  for (const method of ["log", "info", "warn", "error", "debug", "trace"] as const) {
    vi.spyOn(console, method).mockImplementation(record);
  }
  for (const stream of [process.stdout, process.stderr]) {
    vi.spyOn(stream, "write").mockImplementation((chunk: string | Uint8Array) => {
      lines.push(Buffer.from(chunk).toString("utf8"));
      return true;
    });
  }
  await Effect.runPromise(program);
  vi.restoreAllMocks();
  return lines.join("\n");
}

afterEach(() => {
  vi.restoreAllMocks();
});

it("keeps a session token and an email out of every log when Better Auth cannot reach Postgres", async () => {
  const output = await captureOutput(
    Effect.gen(function* () {
      const auth = yield* Auth;
      // As the /api/auth route serves it: a failure answers through the public boundary.
      const serve = (request: Request) => auth.handler(request).pipe(
        Effect.catch((failure) => Effect.succeed(publicErrorResponse(failure))),
      );
      const session = yield* serve(new Request("http://localhost:3000/api/auth/get-session", {
        headers: { authorization: `Bearer ${FAKE_SESSION_TOKEN}` },
      }));
      expect(session.status).toBe(500);
      const signIn = yield* serve(new Request("http://localhost:3000/api/auth/sign-in/email", {
        method: "POST",
        headers: { "content-type": "application/json", origin: "http://localhost:3000" },
        body: JSON.stringify({ email: FAKE_EMAIL, password: "fake-password" }),
      }));
      expect(signIn.status).toBe(500);
      const lookup = yield* Effect.exit(auth.getSession(
        new Headers({ authorization: `Bearer ${FAKE_SESSION_TOKEN}` }),
        { bearer: true },
      ));
      expect(lookup._tag).toBe("Failure");
      yield* Effect.logError("Session lookup failed.", lookup);
    }).pipe(Effect.provide(unreachableAuth)),
  );

  expect(output).not.toContain(FAKE_SESSION_TOKEN);
  expect(output).not.toContain(FAKE_EMAIL);
  expect(output).toContain("DrizzleQueryError in select session");
}, 30_000);
