import { it } from "@effect/vitest";
import { Effect, Layer } from "effect";
import { expect } from "vitest";
import { analyticsRequest, type RequestKind } from "#/modules/web-analytics/tables";
import { Database } from "#/server/database.server";
import { storeTestCloud } from "#/test/store-cloud";
import { dayOf, ensurePartitions, forgetPartitions } from "./partitions";
import { api, errors, request, type RouteQuery } from "./routes";

const ENVIRONMENT = "00000000-0000-4000-8000-00000000a0f1";
const START = new Date("2026-10-10T00:00:00Z");
const query: RouteQuery = { environmentId: ENVIRONMENT, timezone: "UTC", range: { start: START, end: new Date("2026-10-11T00:00:00Z") } };

type Hit = { path: string; status?: number; durationMs?: number; kind?: RequestKind; service?: string; minute?: number };

const seed = Effect.fn(function* (hits: ReadonlyArray<Hit>) {
  forgetPartitions();
  const services = yield* Layer.build(yield* storeTestCloud({ env: { ANALYTICS_ENABLED: "true" } }));
  const provide = <A, E, R>(effect: Effect.Effect<A, E, R>) => effect.pipe(Effect.provide(services), Effect.orDie);
  yield* provide(Effect.gen(function* () {
    yield* ensurePartitions([dayOf(START)]);
    const { drizzle } = yield* Database;
    yield* drizzle.insert(analyticsRequest).values(hits.map((hit, index) => ({
      environmentId: ENVIRONMENT,
      ts: new Date(START.getTime() + (hit.minute ?? index) * 60_000),
      requestId: `r${index}`,
      service: hit.service ?? "web",
      host: "shop.test",
      method: "GET",
      path: hit.path,
      status: hit.status ?? 200,
      durationMs: hit.durationMs ?? 10,
      bytesIn: 0,
      bytesOut: 0,
      kind: hit.kind ?? "other",
      source: "",
    })));
  }));
  return provide;
});

it.live("ids, dates and hashed asset names fold into one route each", () => Effect.gen(function* () {
  const provide = yield* seed([
    { path: "/users/42" },
    { path: "/users/7?tab=posts" },
    { path: "/orders/0f8fad5b-d9cb-469f-a165-70867728950e/items" },
    { path: "/archive/2026-10-10" },
    { path: "/assets/app-3fa9c1d2.js" },
    { path: "/assets/app-Bx81kQz_.js" },
    { path: "/files/1234567.png" },
    { path: "/blog/hello-world" },
    { path: "/" },
    { path: "" },
  ]);
  const { routes } = yield* provide(api(query));
  expect(Object.fromEntries(routes.map((row) => [row.route, row.requests]))).toEqual({
    "/users/:id": 2,
    "/orders/:id/items": 1,
    "/archive/:id": 1,
    "/assets/:id.js": 2,
    "/files/:id.png": 1,
    "/blog/hello-world": 1,
    "/": 2,
  });
}), 60_000);

it.live("api leaves bots out, counts only 5xx as errors, and can narrow to one Service", () => Effect.gen(function* () {
  const provide = yield* seed([
    { path: "/api/a", status: 200, durationMs: 10 },
    { path: "/api/a", status: 404, durationMs: 20 },
    { path: "/api/a", status: 503, durationMs: 300 },
    { path: "/api/a", status: 500, kind: "bot" },
    { path: "/api/b", status: 200, service: "worker", durationMs: 5 },
  ]);
  const whole = yield* provide(api(query));
  expect(whole.routes.map(({ route, requests, errors }) => ({ route, requests, errors }))).toEqual([
    { route: "/api/a", requests: 3, errors: 1 },
    { route: "/api/b", requests: 1, errors: 0 },
  ]);
  expect(whole.routes[0]?.p50).toBe(20);
  expect(whole.slowest.map((sample) => sample.durationMs)).toEqual([300, 20, 10, 5]);
  const worker = yield* provide(api({ ...query, service: "worker" }));
  expect(worker.routes.map((row) => row.route)).toEqual(["/api/b"]);
}), 60_000);

it.live("routes past the busiest 200 fold into Other", () => Effect.gen(function* () {
  const hits: Hit[] = [{ path: "/busy" }, { path: "/busy" }];
  for (let index = 0; index < 205; index++) hits.push({ path: `/page-${String.fromCharCode(97 + (index % 26))}${index}` });
  const provide = yield* seed(hits);
  const { routes } = yield* provide(api(query));
  expect(routes).toHaveLength(201);
  expect(routes.slice(0, 2)).toMatchObject([{ route: "Other", requests: 6 }, { route: "/busy", requests: 2 }]);
}), 60_000);

it.live("errors splits 4xx from 5xx by hour and route, newest first, and a request reads back by id", () => Effect.gen(function* () {
  const provide = yield* seed([
    { path: "/a", status: 200, minute: 5 },
    { path: "/a", status: 404, minute: 10 },
    { path: "/a", status: 404, minute: 20 },
    { path: "/b/9", status: 502, minute: 70 },
    { path: "/b/9", status: 500, minute: 80, kind: "bot" },
  ]);
  const result = yield* provide(errors(query));
  expect(result.hourly).toEqual([
    { bucket: new Date("2026-10-10T00:00:00Z"), clientErrors: 2, serverErrors: 0 },
    { bucket: new Date("2026-10-10T01:00:00Z"), clientErrors: 0, serverErrors: 1 },
  ]);
  expect(result.byRoute).toEqual([
    { route: "/a", status: 404, requests: 2 },
    { route: "/b/:id", status: 502, requests: 1 },
  ]);
  expect(result.newest.map((sample) => sample.requestId)).toEqual(["r3", "r2", "r1"]);
  expect(yield* provide(request(ENVIRONMENT, "r3"))).toMatchObject({ path: "/b/9", status: 502, service: "web" });
  expect(yield* provide(request(ENVIRONMENT, "missing"))).toBeNull();
}), 60_000);
