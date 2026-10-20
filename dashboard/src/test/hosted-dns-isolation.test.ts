import { assert, describe, it } from "@effect/vitest";
import { ConfigProvider, Effect } from "effect";
import { AppConfig } from "#/server/config.server";
import { testConfigEnvironment } from "#/test/config-environment";

const CLOSED = "http://127.0.0.1:9/";

const hostedDnsUrl = (provider: ConfigProvider.ConfigProvider) =>
  AppConfig.make.pipe(
    Effect.map((config) => config.ployz.hostedDnsUrl.href),
    Effect.provideService(ConfigProvider.ConfigProvider, provider),
  );

describe("Hosted DNS in tests", () => {
  it.effect("an isolated provider built from testConfigEnvironment points at a closed port", () =>
    Effect.gen(function* () {
      const env = { ...testConfigEnvironment(), DATABASE_URL: "postgres://unused" };
      assert.strictEqual(yield* hostedDnsUrl(ConfigProvider.fromEnv({ env })), CLOSED);
    }),
  );

  it.effect("the process environment points at a closed port", () =>
    Effect.gen(function* () {
      assert.strictEqual(process.env["PLOYZ_HOSTED_DNS_URL"], CLOSED);
      assert.strictEqual(yield* hostedDnsUrl(ConfigProvider.fromEnv()), CLOSED);
    }),
  );

  it.effect("a provider that names the real Hosted DNS cannot reach it", () =>
    Effect.gen(function* () {
      const env = { ...testConfigEnvironment(), DATABASE_URL: "postgres://unused", PLOYZ_HOSTED_DNS_URL: "https://dns.ployz.app/" };
      const endpoint = yield* hostedDnsUrl(ConfigProvider.fromEnv({ env }));
      const failure = yield* Effect.flip(Effect.tryPromise(() => fetch(new URL("v1/names", endpoint), { method: "POST" })));
      assert.include(String(failure.cause), "Tests must not reach the real Hosted DNS");
    }),
  );

  it.effect.each(["https://dns.ployz.app./", "https://DNS.Ployz.App/", "https://api.dns.ployz.app/"])(
    "the guard also refuses %s",
    (endpoint) =>
      Effect.gen(function* () {
        const failure = yield* Effect.flip(Effect.tryPromise(() => fetch(new URL("v1/names", endpoint), { method: "POST" })));
        assert.include(String(failure.cause), "Tests must not reach the real Hosted DNS");
      }),
  );
});
