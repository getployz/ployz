import { assert, describe, it } from "@effect/vitest";
import { Effect, Fiber } from "effect";
import { TestClock } from "effect/testing";
import { fetchMarketing } from "#/server/marketing-proxy.server";

const marketing = new URL("http://ployz-site.internal");

const visitor = (path: string, init: RequestInit = {}) =>
  new Request(`http://app.internal${path}`, {
    ...init,
    headers: {
      accept: "text/html",
      "accept-language": "en",
      "user-agent": "test-agent",
      cookie: "better-auth.session_token=secret",
      authorization: "Bearer secret",
      "x-forwarded-for": "203.0.113.9",
      "x-forwarded-host": "ployz.app",
      "x-forwarded-proto": "https",
      ...init.headers,
    },
  });

const respond = (response: Response) => {
  const calls: Array<{ url: string; init: RequestInit }> = [];
  const fetchImpl = (url: string, init: RequestInit) => {
    calls.push({ url, init });
    return Promise.resolve(response);
  };
  return { calls, fetchImpl };
};

describe("fetchMarketing", () => {
  it.effect("streams the page back with safe headers and forwards only safe visitor headers", () =>
    Effect.gen(function* () {
      const { calls, fetchImpl } = respond(
        new Response("<h1>Ployz</h1>", {
          status: 200,
          headers: {
            "content-type": "text/html",
            "cache-control": "public, max-age=60",
            etag: '"v1"',
            "last-modified": "Thu, 01 Oct 2026 00:00:00 GMT",
            vary: "accept-encoding",
            link: '</index.md>; rel="alternate"; type="text/markdown"',
            "x-robots-tag": "noindex",
            "content-encoding": "gzip",
            "set-cookie": "tracker=1",
            "x-powered-by": "astro",
          },
        }),
      );
      const response = yield* fetchMarketing(visitor("/blog/x?ref=1"), marketing, fetchImpl);

      assert.strictEqual(calls[0]?.url, "http://ployz-site.internal/blog/x?ref=1");
      assert.deepStrictEqual(Object.fromEntries(new Headers(calls[0]?.init.headers)), {
        accept: "text/html",
        "accept-language": "en",
        "user-agent": "test-agent",
        "x-forwarded-for": "203.0.113.9",
        "x-forwarded-host": "ployz.app",
        "x-forwarded-proto": "https",
      });
      assert.strictEqual(calls[0]?.init.redirect, "manual");
      assert.strictEqual(response?.status, 200);
      assert.deepStrictEqual(Object.fromEntries(response?.headers ?? []), {
        "content-type": "text/html",
        "cache-control": "public, max-age=60",
        etag: '"v1"',
        "last-modified": "Thu, 01 Oct 2026 00:00:00 GMT",
        vary: "accept-encoding",
        link: '</index.md>; rel="alternate"; type="text/markdown"',
        "x-robots-tag": "noindex",
      });
      assert.strictEqual(yield* Effect.promise(() => response?.text() ?? Promise.resolve("")), "<h1>Ployz</h1>");
    }),
  );

  it.effect("derives the forwarded host and proto from the request when no proxy set them", () =>
    Effect.gen(function* () {
      const { calls, fetchImpl } = respond(new Response("ok"));
      yield* fetchMarketing(new Request("http://localhost:5393/"), marketing, fetchImpl);
      const headers = new Headers(calls[0]?.init.headers);
      assert.strictEqual(headers.get("x-forwarded-host"), "localhost:5393");
      assert.strictEqual(headers.get("x-forwarded-proto"), "http");
    }),
  );

  it.effect("passes other error statuses through", () =>
    Effect.gen(function* () {
      const { fetchImpl } = respond(new Response("down", { status: 503 }));
      const response = yield* fetchMarketing(visitor("/"), marketing, fetchImpl);
      assert.strictEqual(response?.status, 503);
    }),
  );

  it.effect("falls back to the app on a marketing 404 or a failed fetch", () =>
    Effect.gen(function* () {
      const notFound = respond(new Response("missing", { status: 404 }));
      assert.isUndefined(yield* fetchMarketing(visitor("/nope"), marketing, notFound.fetchImpl));
      const failing = () => Promise.reject(new TypeError("fetch failed"));
      assert.isUndefined(yield* fetchMarketing(visitor("/nope"), marketing, failing));
    }),
  );

  it.effect("falls back to the app when the marketing site takes over five seconds", () =>
    Effect.gen(function* () {
      const hanging = () => new Promise<Response>(() => {});
      const fiber = yield* Effect.forkChild(fetchMarketing(visitor("/"), marketing, hanging));
      yield* TestClock.adjust("5 seconds");
      assert.isUndefined(yield* Fiber.join(fiber));
    }),
  );

  it.effect("proxies only GET and HEAD", () =>
    Effect.gen(function* () {
      const { calls, fetchImpl } = respond(new Response("ok"));
      assert.isUndefined(yield* fetchMarketing(visitor("/", { method: "POST" }), marketing, fetchImpl));
      assert.strictEqual(calls.length, 0);
      const head = yield* fetchMarketing(visitor("/", { method: "HEAD" }), marketing, fetchImpl);
      assert.strictEqual(head?.status, 200);
      assert.strictEqual(calls[0]?.init.method, "HEAD");
    }),
  );

  it.effect("keeps redirects within the marketing site on the visitor's origin", () =>
    Effect.gen(function* () {
      const redirect = (location: string) =>
        respond(new Response(null, { status: 301, headers: { location } })).fetchImpl;
      const locationOf = (location: string) =>
        fetchMarketing(visitor("/old"), marketing, redirect(location)).pipe(
          Effect.map((response) => response?.headers.get("location")),
        );

      assert.strictEqual(yield* locationOf("http://ployz-site.internal/blog/new?a=1#top"), "/blog/new?a=1#top");
      assert.strictEqual(yield* locationOf("/blog/new"), "/blog/new");
      assert.strictEqual(yield* locationOf("https://github.com/ployz"), "https://github.com/ployz");
    }),
  );
});
