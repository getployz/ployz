import { it } from "@effect/vitest";
import { Deferred, Effect, Exit, Fiber } from "effect";
import { expect } from "vitest";
import { type Attempt, collectGarbage, locked, mint, PublishRefused, read, type Reader, type Writer } from "#/modules/web-analytics/clickhouse/attempt";
import { ClickHouseFailure, type ClickHouseService } from "#/modules/web-analytics/clickhouse/client";
import { Database, type DatabaseService } from "#/server/database.server";
import { silentClickHouse as silent } from "#/test/clickhouse";

declare const n: number;
declare const writer: Writer;

it("an Attempt is minted, never cast, and a publication names one of the three kinds in full", () => {
  // @ts-expect-error a number is not an Attempt
  void (() => { const attempt: Attempt = n; return attempt; });
  // @ts-expect-error an append names the days it dirtied
  void (() => writer.publish({ kind: "append" }));
  // @ts-expect-error a day publication says whether it closes the day
  void (() => writer.publish({ kind: "day", day: "2026-10-01" }));
  void (() => writer.publish({ kind: "refloor" }));
});

const SITE = { rolled_up_to: null, dirty: [], floor: "1", below: "1", pending: [], days: {} };

const answering = (rows: () => ReadonlyArray<object>): DatabaseService => {
  const drizzle = Object.assign({} as DatabaseService["drizzle"], { execute: () => Effect.sync(rows) });
  const service: DatabaseService = {
    drizzle,
    inTransaction: false,
    afterCommit: () => Effect.void,
    transaction: (program) => program.pipe(Effect.provideService(Database, { ...service, inTransaction: true })),
  };
  return service;
};

const insideTransaction: DatabaseService = { ...answering(() => []), inTransaction: true };

const defect = <A, E, R>(effect: Effect.Effect<A, E, R>) =>
  effect.pipe(Effect.catchDefect((cause) => Effect.fail(String(cause))), Effect.flip, Effect.map(String));

const withDatabase = (database: DatabaseService) => <A, E, R>(effect: Effect.Effect<A, E, R>) => Effect.provideService(effect, Database, database);

it.effect("mint refuses to run inside a caller's transaction, so an attempt is pending before any row carries it", () =>
  Effect.gen(function* () {
    expect(yield* defect(mint(silent, ["site"], "append")).pipe(withDatabase(insideTransaction))).toContain("mint commits on its own");
  }));

it.effect("garbage collection refuses to run inside a caller's transaction, so its fence commits before it deletes", () =>
  Effect.gen(function* () {
    expect(yield* defect(collectGarbage(silent, 1)).pipe(withDatabase(insideTransaction))).toContain("collectGarbage commits on its own");
  }));

it.effect("the locked snapshot exists only inside the transaction that will publish against it", () =>
  Effect.gen(function* () {
    expect(yield* defect(locked(silent, "site", () => Effect.void)).pipe(withDatabase(answering(() => [])))).toContain("inside the transaction that publishes");
  }));

it.effect("a read refuses to run inside a transaction, whose snapshot would hide the generation bump it checks for", () =>
  Effect.gen(function* () {
    expect(yield* defect(read(silent, "site", () => Effect.void)).pipe(withDatabase(insideTransaction))).toContain("must not run inside one");
  }));

it.effect("a Reader that escapes its callback answers no further queries, so no result can outlive its validation", () =>
  Effect.gen(function* () {
    const database = answering(() => [{ ...SITE, generation: "3" }]);
    const count = (reader: Reader) => reader.select("SELECT count() AS n FROM pageview", {});
    const escaped = yield* read(silent, "site", Effect.succeed).pipe(withDatabase(database));
    expect(yield* defect(count(escaped))).toContain("only inside the read or locked callback");
    const held = yield* database.transaction(locked(silent, "site", (reader) => Effect.as(count(reader), reader)));
    if (held === null) return yield* Effect.die("the answering database had no site row");
    expect(yield* defect(count(held))).toContain("only inside the read or locked callback");
  }));

const minted = (clickhouse: ClickHouseService, database: DatabaseService) => Effect.gen(function* () {
  const writer = (yield* mint(clickhouse, ["site"], "append")).get("site");
  if (writer === undefined) return yield* Effect.die("the answering database minted nothing");
  return writer;
}).pipe(withDatabase(database));

const publishRow = () => [{ environment_id: "site", attempt: "7", fence: "0" }];

it.effect("a writer behind the garbage collection fence refuses to write, so a stalled attempt cannot land rows after its slot is freed", () =>
  Effect.gen(function* () {
    const database = answering(() => [{ environment_id: "site", attempt: "7", fence: "8" }]);
    const writer = yield* minted(silent, database);
    const refused = yield* Effect.flip(writer.rows("pageview", [{}]));
    expect(refused).toBeInstanceOf(ClickHouseFailure);
    expect(refused.message).toContain("behind the garbage collection fence");
    expect(yield* Effect.flip(database.transaction(writer.publish({ kind: "append", dirty: [] })))).toBeInstanceOf(PublishRefused);
  }));

it.effect("a writer accepts rows only before it publishes, so nothing can land under a published attempt", () =>
  Effect.gen(function* () {
    const database = answering(publishRow);
    const writer = yield* minted(silent, database);
    yield* database.transaction(writer.publish({ kind: "append", dirty: [] }));
    expect(yield* defect(writer.rows("pageview", [{}]))).toContain("already published");
    expect(yield* defect(writer.select([{ query: "INSERT INTO daily (attempt) SELECT {attempt:UInt64}", params: {} }]))).toContain("already published");
    expect(yield* defect(database.transaction(writer.publish({ kind: "refloor" })))).toContain("cannot publish twice");
  }));

it.effect("a writer publishes only inside a transaction", () =>
  Effect.gen(function* () {
    const database = answering(publishRow);
    const writer = yield* minted(silent, database);
    expect(yield* defect(writer.publish({ kind: "refloor" })).pipe(withDatabase(database))).toContain("inside the transaction");
  }));

it.effect("a writer whose write failed is refused publication, so unacknowledged rows never become visible", () =>
  Effect.gen(function* () {
    const database = answering(publishRow);
    const lost = new ClickHouseFailure({ message: "socket hang up", cause: null });
    const writer = yield* minted({ ...silent, unchecked: { ...silent.unchecked, insert: () => Effect.fail(lost) } }, database);
    expect(yield* Effect.flip(writer.rows("pageview", [{}]))).toBe(lost);
    expect(yield* Effect.flip(database.transaction(writer.publish({ kind: "append", dirty: [] })))).toBeInstanceOf(PublishRefused);
  }));

it.effect("a writer with a write still in flight cannot publish, and the write that was interrupted poisons it", () =>
  Effect.gen(function* () {
    const database = answering(publishRow);
    const started = yield* Deferred.make<void>();
    const release = yield* Deferred.make<void>();
    const slow: ClickHouseService = { ...silent, unchecked: { ...silent.unchecked, insert: () => Deferred.succeed(started, undefined).pipe(Effect.andThen(Deferred.await(release))) } };
    const writer = yield* minted(slow, database);
    const write = yield* Effect.forkChild(writer.rows("pageview", [{}]));
    yield* Deferred.await(started);
    expect(yield* defect(database.transaction(writer.publish({ kind: "append", dirty: [] })))).toContain("still in flight");
    yield* Deferred.succeed(release, undefined);
    expect(Exit.isSuccess(yield* Fiber.await(write))).toBe(true);
  }));

it.effect("Writer.select takes only inserts that stamp the attempt column, so rollups cannot land under an unminted attempt", () =>
  Effect.gen(function* () {
    const database = answering(publishRow);
    const commands: string[] = [];
    const recording: ClickHouseService = { ...silent, unchecked: { ...silent.unchecked, command: (query) => Effect.sync(() => { commands.push(query); }) } };
    const writer = yield* minted(recording, database);
    const stamped = "INSERT INTO daily (environment_id, day, attempt, dim) SELECT environment_id, day, {attempt:UInt64}, dim FROM pageview";
    yield* writer.select([{ query: stamped, params: {} }]);
    expect(commands).toEqual([stamped]);
    expect(yield* defect(writer.select([{ query: "INSERT INTO daily (environment_id, day, dim) SELECT environment_id, day, dim FROM pageview", params: {} }]))).toContain("{attempt:UInt64}");
    expect(yield* defect(writer.select([{ query: "INSERT INTO daily (environment_id, day, attempt, dim) SELECT environment_id, day, 7, dim FROM pageview", params: {} }]))).toContain("{attempt:UInt64}");
  }));

it.effect("a read returns its body's result when the site's generation held, and retries a bounded number of times when it moved", () =>
  Effect.gen(function* () {
    let ran = 0;
    const body = () => Effect.sync(() => ++ran);
    const steady = answering(() => [{ ...SITE, generation: "3" }]);
    expect(yield* read(silent, "site", body).pipe(withDatabase(steady))).toBe(1);
    let generation = 0;
    const moving = answering(() => [{ ...SITE, generation: String(generation++) }]);
    const failure = yield* Effect.flip(read(silent, "site", body)).pipe(withDatabase(moving));
    expect(failure).toBeInstanceOf(ClickHouseFailure);
    expect(failure.message).toContain("Garbage collection raced 4 reads");
    expect(ran).toBe(5);
  }));
