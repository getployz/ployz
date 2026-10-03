import { describe, expect, it } from "vitest";
import { ConfigProvider, Effect } from "effect";
import { PostHog, PostHogLive } from "#/modules/analytics/posthog.server";
import { AppConfig } from "#/server/config.server";

describe("PostHogLive", () => {
  it("is the no-op service on a Cloud without POSTHOG_KEY", async () => {
    // As every Self-hosted Cloud is started.
    const config = AppConfig.make.pipe(Effect.provideService(
      ConfigProvider.ConfigProvider,
      ConfigProvider.fromEnv({ env: {
        DATABASE_URL: "postgres://postgres:postgres@localhost:5432/ployz_cloud",
        APP_URL: "http://localhost:3000",
        BETTER_AUTH_SECRET: "better-auth-secret",
        GITHUB_CLIENT_ID: "github-client-id",
        GITHUB_CLIENT_SECRET: "github-client-secret",
        GITHUB_APP_ID: "12345",
        GITHUB_APP_PRIVATE_KEY: "github-app-private-key",
        GITHUB_APP_SLUG: "ployz-test",
        GITHUB_APP_WEBHOOK_SECRET: "github-app-webhook-secret",
        INNGEST_EVENT_KEY: "inngest-event-key",
        INNGEST_SIGNING_KEY: "inngest-signing-key",
        APP_ENCRYPTION_SECRET: "app-encryption-secret-at-least-32-characters",
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
