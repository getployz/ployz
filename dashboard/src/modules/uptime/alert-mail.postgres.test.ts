import { randomUUID } from "node:crypto";
import { it } from "@effect/vitest";
import { InngestTestEngine } from "@inngest/test";
import type { JsonValue } from "@ployz/sdk";
import { sql, type SQL } from "drizzle-orm";
import { Effect, Layer } from "effect";
import { Inngest } from "inngest";
import { expect, vi } from "vitest";
import { cloudStore, type CloudStore } from "#/modules/config-store/store-sdk.server";
import type { InngestClient } from "#/modules/inngest/client";
import { member, user } from "#/modules/identity/tables";
import type { MailSendRequestedEventData } from "#/modules/inngest/events";
import { notificationPreference } from "#/modules/notifications/tables";
import { createSendUptimeAlerts } from "#/modules/uptime/alert-mail.inngest";
import { sendUptimeAlerts } from "#/modules/uptime/alert-mail.server";
import { controlPlaneStatements } from "#/modules/uptime/incident-fence";
import type { AppConfig } from "#/server/config.server";
import { Database } from "#/server/database.server";
import { seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000e401";
const PROJECT = "00000000-0000-4000-8000-00000000e402";
const ENVIRONMENT = "00000000-0000-4000-8000-00000000e403";
const SERVICE = "00000000-0000-4000-8000-00000000e404";

const evidence = {
  quorum: { distinctRegions: 2 },
  observations: [
    { region: "fra", kind: "confirm", verdict: "fail", status: 502, failedPhase: null, error: null, remoteIp: null, at: "2026-10-14T03:02:00.000Z" },
  ],
};

const literal = (value: unknown) => `'${(typeof value === "string" ? value : JSON.stringify(value)).replaceAll("'", "''")}'`;

/**
 * Cloud with the Store open: Organization Acme whose members are Ada and Bob, Bob with uptime email off, and a check on
 * Service `web` in shop / production. `open` and `close` write incidents and their alerts with the control plane's own
 * statements. `mails` are the mail/send events queued so far.
 */
const setup = Effect.fn(function* () {
  const inngest = new Inngest({ id: "alert-mail-test" });
  const send = vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });
  const services = yield* Layer.build(yield* storeTestCloud({ inngest, env: { APP_URL: "https://cloud.example.com" } }));
  const provide = <A, E>(effect: Effect.Effect<A, E, Database | AppConfig | InngestClient | CloudStore>) =>
    effect.pipe(Effect.provide(services));
  yield* provide(seedStoreOrganization(ORGANIZATION, { name: "Acme", slug: "acme" }));
  const store = yield* provide(cloudStore);
  yield* Effect.promise(() => store.write(ORGANIZATION, { command: "create_project", id: PROJECT, name: "shop", default_environment: ENVIRONMENT }));

  const query = <Row extends Record<string, JsonValue>>(statement: SQL) => provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    return yield* drizzle.execute<Row>(statement, "objects");
  }).pipe(Effect.orDie));
  yield* provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const [bob] = yield* drizzle.insert(user).values({ email: "bob@acme.test", name: "Bob" }).returning();
    if (bob === undefined) throw new Error("no Bob");
    yield* drizzle.insert(member).values({ userId: bob.id, organizationId: ORGANIZATION, role: "member" });
    yield* drizzle.insert(notificationPreference).values({ userId: bob.id, type: "uptime", channel: "none" });
  }).pipe(Effect.orDie));
  const [check] = yield* query<{ id: string }>(sql`
    insert into uptime_check (organization_id, name, url, environment_id, service_id)
    values (${ORGANIZATION}, 'web', 'https://web.acme.test/health', ${ENVIRONMENT}, ${SERVICE}) returning id`);
  if (check === undefined) throw new Error("no check");

  const statement = (text: string, values: readonly unknown[]) =>
    query(sql.raw(text.replace(/\$(\d)/g, (_, n: string) => literal(values[Number(n) - 1]))));
  const open = Effect.fn(function* (openedAt: string) {
    const id = randomUUID();
    yield* statement(controlPlaneStatements.open, [check.id, 1, id, openedAt, evidence, { transition: "opened" }]);
    return id;
  });
  const close = (id: string, closedAt: string) =>
    statement(controlPlaneStatements.observedClose, [id, 1, closedAt, { kind: "observed" }, { transition: "resolved" }]);
  /** Age every alert of `incident` as if the control plane wrote it at `at`. */
  const writtenAt = (incident: string, at: string) =>
    query(sql`update uptime_alert set created_at = ${at}::timestamptz where incident_id = ${incident}`);
  const alerts = (incident: string) => query<{ kind: string; state: string }>(sql`
    select kind, state from uptime_alert where incident_id = ${incident} order by kind`);
  const mails = () => send.mock.calls.flatMap(([event]) => (Array.isArray(event) ? event : [event]))
    .map((event) => ({ id: event.id, data: event.data as MailSendRequestedEventData }));
  const run = (now: string) => provide(sendUptimeAlerts(new Date(now)));
  return { open, close, writtenAt, alerts, mails, run, provide, send };
});

it.live("an opened incident mails every member who wants uptime email, once, and skips a member set to None", () => Effect.gen(function* () {
  const { open, alerts, mails, run } = yield* setup();
  const incident = yield* open("2026-10-14T03:02:00Z");

  yield* run("2026-10-14T03:03:00Z");
  yield* run("2026-10-14T03:04:00Z");

  expect(yield* alerts(incident)).toEqual([{ kind: "opened", state: "sent" }]);
  const sent = mails();
  expect(sent.map((mail) => mail.data.to)).toEqual(["ada@example.test"]);
  const [down] = sent;
  expect(down?.id).toMatch(new RegExp(`^mail-uptime:${incident}:opened:`));
  expect(down?.data.subject).toBe("Down: web in shop / production");
  expect(down?.data.text).toContain("502 Bad Gateway");
  expect(down?.data.text).toContain("GET https://web.acme.test/health");
  expect(down?.data.text).toContain(`https://cloud.example.com/cloud/acme/shop/production/services/${SERVICE}?tab=monitoring&from=2026-10-14T02%3A02%3A00.000Z`);
  expect(down?.data.headers?.["List-Unsubscribe"]).toMatch(/^<https:\/\/cloud\.example\.com\/api\/mail\/unsubscribe\?token=/);
}));

it.live("a recovered incident mails after its down mail, naming how long it was down", () => Effect.gen(function* () {
  const { open, close, alerts, mails, run } = yield* setup();
  const incident = yield* open("2026-10-14T03:02:00Z");
  yield* close(incident, "2026-10-14T03:14:00Z");

  yield* run("2026-10-14T03:15:00Z");

  expect(yield* alerts(incident)).toEqual([{ kind: "opened", state: "sent" }, { kind: "resolved", state: "sent" }]);
  expect(mails().map((mail) => mail.data.subject)).toEqual(["Down: web in shop / production", "Recovered: web in shop / production"]);
  const recovered = mails()[1]?.data;
  expect(recovered?.text).toContain("web was down for 12 minutes.");
  expect(recovered?.text).toContain("03:02 to 03:14 UTC");
}));

it.live("an alert written more than an hour ago expires without mail, and its recovery never mails either", () => Effect.gen(function* () {
  const { open, close, writtenAt, alerts, mails, run } = yield* setup();
  const incident = yield* open("2026-10-14T01:00:00Z");
  yield* writtenAt(incident, "2026-10-14T01:00:00Z");

  yield* run("2026-10-14T02:01:00Z");
  yield* close(incident, "2026-10-14T02:05:00Z");
  yield* run("2026-10-14T02:06:00Z");

  expect(yield* alerts(incident)).toEqual([{ kind: "opened", state: "expired" }, { kind: "resolved", state: "expired" }]);
  expect(mails()).toEqual([]);
}));

it.live("two runs at once claim each alert once", () => Effect.gen(function* () {
  const { open, close, alerts, mails, run } = yield* setup();
  const incidents = yield* Effect.forEach(["03:01", "03:02", "03:03"], (minute) => Effect.gen(function* () {
    const incident = yield* open(`2026-10-14T${minute}:00Z`);
    yield* close(incident, `2026-10-14T${minute}:30Z`);
    return incident;
  }));

  const claimed = yield* Effect.all([run("2026-10-14T03:05:00Z"), run("2026-10-14T03:05:00Z")], { concurrency: "unbounded" });

  expect(claimed[0] + claimed[1]).toBe(6);
  for (const incident of incidents) {
    expect(yield* alerts(incident)).toEqual([{ kind: "opened", state: "sent" }, { kind: "resolved", state: "sent" }]);
  }
  expect(new Set(mails().map((mail) => mail.id)).size).toBe(6);
  expect(mails()).toHaveLength(6);
}));

it.live("the cron function sends what is pending", () => Effect.gen(function* () {
  const { open, alerts, provide } = yield* setup();
  const incident = yield* open(new Date().toISOString());
  const fn = createSendUptimeAlerts(new Inngest({ id: "alert-mail-cron-test" }), (program) => Effect.runPromise(provide(program)));

  yield* Effect.promise(() => new InngestTestEngine({ function: fn, events: [{ name: "inngest/scheduled.timer", data: {} }] }).execute());

  expect(yield* alerts(incident)).toEqual([{ kind: "opened", state: "sent" }]);
}));
