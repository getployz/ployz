import { randomUUID } from "node:crypto";
import { Effect } from "effect";
import { inject } from "vitest";
import { clickHouseClient, makeClickHouse } from "#/modules/web-analytics/clickhouse/client";
import { ensureSchema } from "#/modules/web-analytics/clickhouse/schema";

export const clickhouseTestDatabase = Effect.gen(function* () {
  const name = `test_${randomUUID().replaceAll("-", "")}`;
  const admin = makeClickHouse(yield* clickHouseClient(inject("clickhouseUrl")));
  yield* admin.command(`CREATE DATABASE ${name}`);
  yield* Effect.addFinalizer(() => admin.command(`DROP DATABASE IF EXISTS ${name} SYNC`).pipe(Effect.ignore));
  const url = new URL(inject("clickhouseUrl"));
  url.pathname = `/${name}`;
  const clickhouse = makeClickHouse(yield* clickHouseClient(url.href));
  yield* ensureSchema(clickhouse);
  return { url, clickhouse };
});
