import { afterEach, describe, expect, it, vi } from "vitest";
import { ConfigProvider, Effect, Layer } from "effect";
import { GithubApi, GithubApiLive, resolveGithubRepository } from "#/modules/github/github-observation.api";
import { getGithubAppInstallUrl } from "#/modules/github/github.server";
import { AppConfig } from "#/server/config.server";
import { testConfigEnvironment } from "#/test/config-environment";

const sha = "a".repeat(40);

function configured(origin: { readonly PLOYZ_GITHUB_ORIGIN?: string }) {
  const env = { ...testConfigEnvironment(), DATABASE_URL: "postgres://unused", GITHUB_APP_SLUG: "ployz-test", ...origin };
  const config = Layer.effect(AppConfig, AppConfig.make).pipe(
    Layer.provide(ConfigProvider.layer(ConfigProvider.fromEnv({ env }))),
  );
  return GithubApiLive.pipe(Layer.provideMerge(config));
}

afterEach(() => vi.unstubAllGlobals());

describe("GitHub origins", () => {
  it.each([
    { name: "the real hosts when PLOYZ_GITHUB_ORIGIN is unset", origin: {},
      api: "https://api.github.com", web: "https://github.com", codeload: "https://codeload.github.com" },
    { name: "PLOYZ_GITHUB_ORIGIN for every host when it is set", origin: { PLOYZ_GITHUB_ORIGIN: "http://127.0.0.1:4555" },
      api: "http://127.0.0.1:4555", web: "http://127.0.0.1:4555", codeload: "http://127.0.0.1:4555" },
  ])("builds every URL from $name", async ({ origin, api, web, codeload }) => {
    const archive = `${codeload}/acme/web/legacy.tar.gz/${sha}`;
    const fetchMock = vi.fn<typeof fetch>()
      .mockResolvedValueOnce(Response.json({ token: "installation-secret", expires_at: "2099-01-01T00:00:00Z" }))
      .mockResolvedValueOnce(Response.json({ id: 42, full_name: "acme/web" }))
      .mockResolvedValueOnce(new Response(null, { status: 302, headers: { location: archive } }))
      .mockResolvedValueOnce(new Response("tarball"));
    vi.stubGlobal("fetch", fetchMock);

    const installUrl = await Effect.runPromise(Effect.gen(function* () {
      const repository = yield* resolveGithubRepository(7, 42);
      const response = yield* (yield* GithubApi).archive({ installationId: 7, repository, sha });
      expect(yield* Effect.promise(() => response.text())).toBe("tarball");
      return getGithubAppInstallUrl((yield* AppConfig).github);
    }).pipe(Effect.provide(configured(origin))));

    expect(fetchMock.mock.calls.map(([url]) => String(url))).toEqual([
      `${api}/app/installations/7/access_tokens`,
      `${api}/repositories/42`,
      `${api}/repos/acme/web/tarball/${sha}`,
      archive,
    ]);
    expect(installUrl).toBe(`${web}/apps/ployz-test/installations/new`);
  });
});
