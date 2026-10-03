import { describe, expect, it } from "vitest";
import { ConfigProvider, Effect } from "effect";
import { PostHog, PostHogLive } from "#/modules/analytics/posthog.server";
import { AppConfig } from "#/server/config.server";
import { testConfigEnvironment } from "#/test/config-environment";

describe("PostHogLive", () => {
  it("is the no-op service on a Cloud without POSTHOG_KEY", async () => {
    // As every Self-hosted Cloud is started.
    const config = AppConfig.make.pipe(Effect.provideService(
      ConfigProvider.ConfigProvider,
      ConfigProvider.fromEnv({ env: {
        ...testConfigEnvironment(),
        DATABASE_URL: "postgres://unused",
      } }),
    ));
    const live = await Effect.runPromise(
      Effect.scoped(Effect.gen(function* () { return yield* PostHog; })).pipe(
        Effect.provide(PostHogLive),
        Effect.provideServiceEffect(AppConfig, config),
      ),
    );

    // No SDK client exists to send anything.
    expect(live).toBe(await Effect.runPromise(Effect.gen(function* () { return yield* PostHog; })));
  });
});
