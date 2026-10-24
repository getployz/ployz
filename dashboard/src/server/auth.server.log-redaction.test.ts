import { testConfigEnvironment } from "#/test/config-environment";
import { createAdapterFactory } from "@better-auth/core/db/adapter";
import { INVALID_SPAN_CONTEXT, type Span, trace, type Tracer } from "@opentelemetry/api";
import { DrizzleQueryError } from "drizzle-orm";
import { ConfigProvider, Effect, Layer } from "effect";
import { Inngest } from "inngest";
import { formatWithOptions } from "node:util";
import { afterEach, expect, it, vi } from "vitest";
import { Polar } from "#/modules/billing/polar-provider.server";
import { InngestClient } from "#/modules/inngest/client";
import { Auth, AuthLive, logBetterAuth } from "#/server/auth.server";
import { AppConfig } from "#/server/config.server";
import { DatabaseLive } from "#/server/database.server";
import { RedactedLogsLive } from "#/server/log-redaction";
import { publicErrorResponse } from "#/server/public-error";

const FAKE_SESSION_TOKEN = "fake-session-token-0123456789abc";
const FAKE_EMAIL = "fake-user@example.test";
const FAKE_TOKEN_HASH = "fake-refresh-token-hash-0123456789";
const FAKE_USER_ID = "00000000-0000-4000-8000-00000fa4e1d0";

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

/** Everything written to the console, stdio or an OpenTelemetry span while `program` runs. */
async function captureOutput<E>(program: Effect.Effect<void, E>) {
  const lines: string[] = [];
  const record = (...args: unknown[]) => {
    lines.push(formatWithOptions({ depth: Infinity }, ...args));
  };
  const span: Span = new Proxy(trace.wrapSpanContext(INVALID_SPAN_CONTEXT), {
    get: (target, key) => key === "spanContext" || key === "isRecording"
      ? () => key === "isRecording" || target.spanContext()
      : (...args: unknown[]) => {
        record("span", String(key), ...args);
        return span;
      },
  });
  const tracer: Tracer = {
    startSpan: () => span,
    startActiveSpan: (...args: unknown[]) => {
      const run = args.at(-1);
      return run instanceof Function ? run(span) : undefined;
    },
  };
  trace.setGlobalTracerProvider({ getTracer: () => tracer });
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
  trace.disable();
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

it("names an error Better Auth passes as the message by its kind", async () => {
  const output = await captureOutput(Effect.sync(() => {
    logBetterAuth("error", new DrizzleQueryError('select "id" from "session" where "user_id" = $1', [FAKE_SESSION_TOKEN]));
  }));

  expect(output).not.toContain(FAKE_SESSION_TOKEN);
  expect(output).toContain("DrizzleQueryError in select session");
});

it("keeps a hashed token out of the log when OAuth introspection's query fails", async () => {
  const failure = new DrizzleQueryError('select "id" from "oauth_refresh_token" where "token" = $1', [FAKE_TOKEN_HASH]);
  // As @better-auth/oauth-provider logs an introspection failure: its message and stack, as strings.
  const output = await captureOutput(Effect.sync(() => {
    logBetterAuth("error", "Introspection error:", failure.message, failure.stack);
  }));

  expect(output).not.toContain(FAKE_TOKEN_HASH);
  expect(output).toContain("Introspection error:");
});

it("keeps a user id out of every log when a session's joined user query fails", async () => {
  const failure = new DrizzleQueryError('select "id" from "user" where "id" = $1', [FAKE_USER_ID]);
  const adapter = createAdapterFactory({
    config: { adapterId: "failing-user-join" },
    adapter: () => ({
      findOne: async <T>({ model }: { model: string }) => {
        if (model === "user") throw failure;
        // The session the join starts from; only its user id matters.
        // oxlint-disable-next-line typescript/no-unsafe-type-assertion
        return { id: "fake-session-id", userId: FAKE_USER_ID } as T;
      },
      findMany: async () => [],
      create: async ({ data }) => data,
      update: async () => null,
      updateMany: async () => 0,
      delete: async () => undefined,
      deleteMany: async () => 0,
      count: async () => 0,
    }),
  })({ logger: { log: logBetterAuth } });

  const output = await captureOutput(Effect.promise(async () => {
    const lookup = adapter.findOne({ model: "session", where: [{ field: "id", value: "fake-session-id" }], join: { user: true } });
    await expect(lookup).rejects.toBe(failure);
  }));

  expect(output).not.toContain(FAKE_USER_ID);
  expect(output).toContain("Failed to query fallback join for model user");
});
