import { it } from "@effect/vitest";
import { asc, sql } from "drizzle-orm";
import { Effect, Layer } from "effect";
import { Inngest } from "inngest";
import { expect, vi } from "vitest";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import type { MailSendRequestedEventData } from "#/modules/inngest/events";
import { claimCrashMail, pruneCrashEvents } from "#/modules/observe-ingest/crash-mail.server";
import { crashEvent } from "#/modules/observe-ingest/tables";
import { Database } from "#/server/database.server";
import { machineId } from "#/test/ids";
import { enrollStoreServer, seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000f301";
const PROJECT = "00000000-0000-4000-8000-00000000f302";
const ENVIRONMENT = "00000000-0000-4000-8000-00000000f303";
const MACHINE = machineId("0");
const WEB = "c".repeat(32);
const NAMES = new Map([[MACHINE, "web-1"]]);

const setup = Effect.fn(function* () {
  const inngest = new Inngest({ id: "crash-mail-test" });
  const send = vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });
  const services = yield* Layer.build(yield* storeTestCloud({ inngest, env: { APP_URL: "https://cloud.example.com" } }));
  const provide = <A, E, R>(effect: Effect.Effect<A, E, R>) => effect.pipe(Effect.provide(services));
  yield* provide(seedStoreOrganization(ORGANIZATION, { name: "Acme", slug: "acme" }));
  yield* provide(enrollStoreServer(ORGANIZATION));
  const store = yield* provide(cloudStore);
  yield* Effect.promise(() => store.write(ORGANIZATION, { command: "create_project", id: PROJECT, name: "shop", default_environment: ENVIRONMENT }));
  const namespace = yield* provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* drizzle.execute(sql`
      insert into config_namespace (organization_id, namespace, environment_id) values (${ORGANIZATION}, 'shop-prod', ${ENVIRONMENT})
      on conflict do nothing`);
    const [row] = yield* drizzle.execute<{ namespace: string }>(sql`
      select namespace from config_namespace where environment_id = ${ENVIRONMENT}`, "objects");
    if (row === undefined) throw new Error("no namespace");
    return row.namespace;
  }).pipe(Effect.orDie));

  let seq = 0n;
  const crash = (at: string, last: { exitCode: number | null; oom: boolean } = { exitCode: 1, oom: false }) =>
    provide(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      seq += 1n;
      yield* drizzle.insert(crashEvent).values({
        organizationId: ORGANIZATION, machineId: MACHINE, seq, serviceId: WEB, serviceName: `${namespace}/web`, replica: "1",
        containerId: "b".repeat(64), deploymentId: null, ...last, occurredAt: new Date(at), receivedAt: new Date(at),
      });
    }).pipe(Effect.orDie));
  const unmailed = () => provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    return (yield* drizzle.select({ seq: crashEvent.seq, mailedAt: crashEvent.mailedAt }).from(crashEvent).orderBy(asc(crashEvent.seq)))
      .filter(({ mailedAt }) => mailedAt === null).map((row) => row.seq);
  }).pipe(Effect.orDie));
  const remaining = () => provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    return (yield* drizzle.select({ seq: crashEvent.seq }).from(crashEvent).orderBy(asc(crashEvent.seq))).map((row) => row.seq);
  }).pipe(Effect.orDie));
  const mails = () => send.mock.calls.flatMap(([event]) => (Array.isArray(event) ? event : [event]))
    .map((event) => ({ id: event.id, data: event.data as MailSendRequestedEventData }));
  const claim = (now: string) => provide(claimCrashMail(ORGANIZATION, WEB, new Date(now), NAMES));
  const prune = (now: string) => provide(pruneCrashEvents(new Date(now)));
  return { crash, unmailed, remaining, mails, claim, prune };
});

it.live("five crashes queue one mail that counts them, naming the service, its Server and the last exit", () => Effect.gen(function* () {
  const { crash, unmailed, mails, claim } = yield* setup();
  for (const minute of ["01", "02", "03", "04"]) yield* crash(`2026-10-15T03:${minute}:00Z`);
  yield* crash("2026-10-15T03:05:00Z", { exitCode: 137, oom: true });

  expect(yield* claim("2026-10-15T03:06:00Z")).toEqual({ kind: "mailed", count: 5 });

  expect(yield* unmailed()).toEqual([]);
  const sent = mails();
  expect(sent).toHaveLength(1);
  const [mail] = sent;
  expect(mail?.id).toMatch(/^mail-crash:[0-9a-f-]{36}:/);
  expect(mail?.data.to).toBe("ada@example.test");
  expect(mail?.data.subject).toBe("web crashed 5 times in shop / production");
  expect(mail?.data.text).toContain("The last replica to crash was on web-1 and ran out of memory.");
  expect(mail?.data.text).toContain("Oct 15, 03:05 UTC");
  expect(mail?.data.text).toContain(`https://cloud.example.com/cloud/acme/shop/production/services/${WEB}?tab=monitoring`);
}));

it.live("a crash within the hour after a mail waits for the hour to pass, then mails alone", () => Effect.gen(function* () {
  const { crash, unmailed, mails, claim } = yield* setup();
  yield* crash("2026-10-15T03:01:00Z");
  expect(yield* claim("2026-10-15T03:02:00Z")).toEqual({ kind: "mailed", count: 1 });
  yield* crash("2026-10-15T03:20:00Z");

  expect(yield* claim("2026-10-15T03:21:00Z")).toEqual({ kind: "wait", until: "2026-10-15T04:02:00.000Z" });
  expect(mails()).toHaveLength(1);
  expect(yield* unmailed()).toEqual([2n]);

  expect(yield* claim("2026-10-15T04:02:00Z")).toEqual({ kind: "mailed", count: 1 });
  expect(mails().map((mail) => mail.data.subject)).toEqual(["web crashed in shop / production", "web crashed in shop / production"]);
  expect(yield* claim("2026-10-15T04:03:00Z")).toEqual({ kind: "none" });
}));

it.live("pruning deletes crashes older than 30 days and keeps the rest", () => Effect.gen(function* () {
  const { crash, remaining, prune } = yield* setup();
  yield* crash("2026-09-14T03:00:00Z");
  yield* crash("2026-09-16T03:00:00Z");

  expect(yield* prune("2026-10-15T03:00:00Z")).toBe(1);
  expect(yield* remaining()).toEqual([2n]);
}));
