import { it } from "@effect/vitest";
import { asc } from "drizzle-orm";
import { Effect, Layer } from "effect";
import { Inngest } from "inngest";
import { expect, vi } from "vitest";
import { crashEvent, observeIngestCursor } from "#/modules/observe-ingest/tables";
import { operationOutbox } from "#/modules/operations/tables";
import type { AuthenticateObservePush } from "#/modules/observe-ingest/ingest.server";
import { handleObservePush } from "#/routes/api/machines/-handlers";
import { Database } from "#/server/database.server";
import { Unauthorized } from "#/server/public-error";
import { machineId } from "#/test/ids";
import { enrollStoreServer, seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000f201";
const MACHINE = machineId("0");
const WEB = "c".repeat(32);
const API = "d".repeat(32);

const marker = (service: string, exitCode: number) => ({
  machine_id: MACHINE,
  container_id: "b".repeat(64),
  service_id: service,
  service: service === WEB ? "prod/web" : "prod/api",
  replica: "1",
  ts: 1_760_000_000_000_000_000,
  kind: "exit",
  exit_code: exitCode,
  oom: false,
  stopped: false,
  future_field: "ignored",
});

type PushFields = {
  readonly stream?: string;
  readonly seq?: string | number;
  readonly events?: ReadonlyArray<{ readonly seq: string; readonly marker: { readonly kind: string } }>;
};

/** A push body as a Machine sends it, with `fields` replacing any of its own. */
const push = (events: ReadonlyArray<readonly [seq: number, service: string, exitCode?: number]>, fields: PushFields = {}) =>
  JSON.stringify({
    stream: "crash",
    seq: String(Math.max(...events.map(([seq]) => seq))),
    dropped: 0,
    events: events.map(([seq, service, exitCode = 1]) => ({ seq: String(seq), marker: marker(service, exitCode) })),
    ...fields,
  });

const trusted: AuthenticateObservePush = (_request, machine) =>
  Effect.succeed({ organizationId: ORGANIZATION, machineId: machineId(machine) });

/** Cloud with Organization Acme and one enrolled Server, and a way to push to it as that Server. */
const setup = Effect.fn(function* () {
  const inngest = new Inngest({ id: "observe-ingest-test" });
  vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });
  const services = yield* Layer.build(yield* storeTestCloud({ inngest }));
  const provide = <A, E>(effect: Effect.Effect<A, E, Database>) => effect.pipe(Effect.provide(services));
  yield* provide(seedStoreOrganization(ORGANIZATION, { name: "Acme", slug: "acme" }));
  yield* enrollStoreServer(ORGANIZATION).pipe(Effect.provide(services));

  const send = (body: string, authenticate: AuthenticateObservePush = trusted) => {
    const request = new Request("https://cloud.example.com/api/machines/x/observe", { method: "POST", body });
    return handleObservePush(request, MACHINE, authenticate).pipe(Effect.provide(services));
  };
  const rows = () => provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    return {
      crashes: (yield* drizzle.select().from(crashEvent).orderBy(asc(crashEvent.seq)))
        .map(({ seq, serviceId, serviceName, exitCode, oom }) => ({ seq, serviceId, serviceName, exitCode, oom })),
      cursor: (yield* drizzle.select().from(observeIngestCursor)).map(({ stream, seq }) => ({ stream, seq })),
      events: (yield* drizzle.select().from(operationOutbox)).map(({ name, data }) => ({ name, data })),
    };
  }).pipe(Effect.orDie));
  return { send, rows };
});

it.live("a push keeps its crashes, advances the cursor, and asks for one mail per Service", () => Effect.gen(function* () {
  const { send, rows } = yield* setup();
  const answer = yield* send(push([[10, WEB], [11, WEB, 0], [12, API, 137]]));
  expect(answer.status).toBe(204);
  expect(answer.headers.get("cache-control")).toBe("no-store");
  expect(yield* rows()).toEqual({
    crashes: [
      { seq: 10n, serviceId: WEB, serviceName: "prod/web", exitCode: 1, oom: false },
      { seq: 12n, serviceId: API, serviceName: "prod/api", exitCode: 137, oom: false },
    ],
    cursor: [{ stream: "crash", seq: 12n }],
    events: expect.arrayContaining([
      { name: "observe/crash.received", data: { organizationId: ORGANIZATION, serviceId: WEB } },
      { name: "observe/crash.received", data: { organizationId: ORGANIZATION, serviceId: API } },
    ]),
  });
  expect((yield* rows()).events).toHaveLength(2);
}), 60_000);

it.live("a replayed seq answers 204 and writes nothing", () => Effect.gen(function* () {
  const { send, rows } = yield* setup();
  expect((yield* send(push([[10, WEB], [12, WEB]]))).status).toBe(204);
  const before = yield* rows();
  expect((yield* send(push([[10, WEB], [12, WEB]]))).status).toBe(204);
  expect((yield* send(push([[11, API]]))).status).toBe(204);
  expect(yield* rows()).toEqual(before);
}), 60_000);

it.live("a batch that straddles the cursor keeps only the crashes it had not kept", () => Effect.gen(function* () {
  const { send, rows } = yield* setup();
  expect((yield* send(push([[10, WEB], [12, WEB]]))).status).toBe(204);
  expect((yield* send(push([[12, WEB], [13, API]]))).status).toBe(204);
  const { crashes, cursor, events } = yield* rows();
  expect(crashes.map(({ seq }) => seq)).toEqual([10n, 12n, 13n]);
  expect(cursor).toEqual([{ stream: "crash", seq: 13n }]);
  expect(events.map(({ data }) => data)).toEqual([
    { organizationId: ORGANIZATION, serviceId: WEB },
    { organizationId: ORGANIZATION, serviceId: API },
  ]);
}), 60_000);

it.live("a stream Cloud does not know answers 404 and writes nothing", () => Effect.gen(function* () {
  const { send, rows } = yield* setup();
  const answer = yield* send(push([[10, WEB]], { stream: "logs" }));
  expect(answer.status).toBe(404);
  expect(yield* rows()).toEqual({ crashes: [], cursor: [], events: [] });
}), 60_000);

it.live("a push the auth seam rejects answers 401 and writes nothing", () => Effect.gen(function* () {
  const { send, rows } = yield* setup();
  const answer = yield* send(push([[10, WEB]]), () => Effect.fail(new Unauthorized()));
  expect(answer.status).toBe(401);
  expect(yield* rows()).toEqual({ crashes: [], cursor: [], events: [] });
}), 60_000);

it.live("a malformed push answers 422 and writes nothing", () => Effect.gen(function* () {
  const { send, rows } = yield* setup();
  for (const body of [
    "not json",
    push([[10, WEB]], { seq: 10 }),
    push([[10, WEB]], { seq: "9999999999999999999" }),
    push([[10, WEB]], { events: [{ seq: "10", marker: { kind: "exit" } }] }),
  ]) {
    expect((yield* send(body)).status).toBe(422);
  }
  expect(yield* rows()).toEqual({ crashes: [], cursor: [], events: [] });
}), 60_000);
