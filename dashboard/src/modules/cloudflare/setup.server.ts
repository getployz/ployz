import "@tanstack/react-start/server-only";
import { and, eq } from "drizzle-orm";
import { Cause, Effect, Exit, type Redacted } from "effect";
import {
  CloudflareError,
  CloudflareHttp,
  checkEditPermissions,
  createRecord,
  createTunnel,
  deleteRecord,
  deleteTunnel,
  findRecords,
  findTunnel,
  listZones,
  refusalWords,
  tunnelToken,
} from "#/modules/cloudflare/api.server";
import { loadCloudflareSetup } from "#/modules/cloudflare/source.server";
import { normalizeZone, wildcardRecord, zoneProblem } from "#/modules/cloudflare/zone";
import { doorsGap, SETUP_RUNNING_LIMIT_MS } from "#/modules/ingress/doors";
import { closedFounding } from "#/modules/ingress/doors.server";
import { activeDoorsAttempt, lockIngressRows } from "#/modules/ingress/doors.lock.server";
import { organizationCloudflareIngress } from "#/modules/ingress/tables";
import { organization } from "#/modules/organization/tables";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import type { PloyzSession } from "#/modules/runtime/ployz.server";
import { Database } from "#/server/database.server";
import { Conflict, Validation } from "#/server/public-error";

/** The Cloudflare Cloud calls through; tests give it a mock of the API. */
export const realCloudflare = new CloudflareHttp({ fetch: (input, init) => fetch(input, init) });

const BOTH = ["public", "tunnel"] as const;
const PUBLIC = ["public"] as const;
const TUNNEL = ["tunnel"] as const;

const refuse = (message: string) => new Validation({ message, userFacing: true });

/** A Cloudflare refusal as the words a user reads, so the token and Cloudflare's detail stay out of the reply. */
const words = <A, E, R>(effect: Effect.Effect<A, E, R>) =>
  effect.pipe(Effect.catchIf(
    (error): error is Extract<E, CloudflareError> => error instanceof CloudflareError,
    (error) => Effect.fail(refuse(refusalWords(error))),
  ));

/** `words`, for the reads before the Cluster is reserved: a refusal there leaves everything as it was. */
const untouched = <A, E, R>(effect: Effect.Effect<A, E, R>) =>
  effect.pipe(words, Effect.mapError((error) => error instanceof Validation ? refuse(`${error.message} Nothing was changed.`) : error));

/** The names of the zones the token can see, so the page can offer them. Nothing is changed. */
export const listCloudflareZoneNames = Effect.fn("Cloudflare.zoneNames")(function* (token: Redacted.Redacted<string>, http: CloudflareHttp = realCloudflare) {
  const zones = yield* listZones(http, token).pipe(words);
  return zones.map((zone) => zone.name).sort();
});

/**
 * Whether `token` can set Cloudflare up on `zone`, asked before the Cluster's first Server exists, so the first-run card
 * can wait on an answer instead of a guess. Nothing is created and the token is not kept: setup asks again with it once
 * the Server joins.
 */
export const checkCloudflareSetup = Effect.fn("Cloudflare.check")(function* (
  input: { readonly token: Redacted.Redacted<string>; readonly zone: string },
  http: CloudflareHttp = realCloudflare,
) {
  const zones = yield* listZones(http, input.token).pipe(words);
  const zoneTrouble = zoneProblem(input.zone, zones.map((zone) => zone.name));
  if (zoneTrouble !== null) return yield* refuse(zoneTrouble);
  const zone = zones.find((candidate) => candidate.name === normalizeZone(input.zone));
  if (zone === undefined) return yield* refuse(`${normalizeZone(input.zone)} isn't a zone in this Cloudflare account.`);
  yield* checkEditPermissions(http, input.token, zone).pipe(words);
  const name = `*.${zone.name}`;
  const there = yield* findRecords(http, input.token, zone.id, name).pipe(words);
  if (there.length > 0) return yield* refuse(`${name} already exists in Cloudflare. Remove that record or pick another zone.`);
  return { zone: zone.name };
});

/** Why the Cluster can't be set up now: it already uses Cloudflare, another setup holds it, or a switch is moving its Doors. */
const setupProblem = Effect.fn("Cloudflare.setupProblem")(function* (organizationId: string) {
  const existing = yield* loadCloudflareSetup(organizationId);
  if (existing?.state === "active") return "This Cluster already uses Cloudflare.";
  if (existing?.state === "setting_up") return "Cloudflare is being set up. Wait for it to finish.";
  const switching = yield* activeDoorsAttempt(organizationId);
  return switching === null ? null : "A switch is under way. Wait for it to finish, then set up.";
});

/** How long opening the ports may hold the lock that setup and a switch wait on. */
const OPEN_PUBLIC_LIMIT_MS = 60_000;

/**
 * Open ports 80 and 443 on a Cluster founded closed whose Cloudflare setup never finished, so its apps answer instead of
 * staying dark. Refused once Cloudflare is set up or in use, while it is being set up, or while a switch runs.
 */
export const openPublicPorts = Effect.fn("Cloudflare.openPublic")(function* (organizationId: string) {
  const database = yield* Database;
  const opened = yield* (yield* OrganizationRuntime).open(organizationId);
  if (opened.status !== "connected") return yield* refuse("Your servers aren't answering. Try again once they are.");
  // Checked and opened under the lock setup reserves with, so no setup lands between the check and the ports opening.
  return yield* database.transaction(Effect.gen(function* () {
    yield* lockIngressRows(organizationId);
    const setup = yield* loadCloudflareSetup(organizationId);
    if (setup?.state === "set_up") return yield* new Conflict({ userFacing: true, message: "Cloudflare Tunnel is set up. Switch web traffic in Settings, Network." });
    const early = yield* setupProblem(organizationId);
    if (early !== null) return yield* new Conflict({ userFacing: true, message: early });
    const answered = yield* opened.connected.setIngressDoors(PUBLIC).pipe(Effect.timeout(OPEN_PUBLIC_LIMIT_MS), Effect.result);
    const gap = answered._tag === "Failure" ? "Your servers didn't answer." : doorsGap(answered.success, PUBLIC);
    if (gap !== null) return yield* refuse(gap);
    return { opened: true } as const;
  }));
}, Effect.scoped);

/** The Doors back to public only, after a failed setup. Why a Server still holds the tunnel, or null once none does. */
const restorePublic = Effect.fn("Cloudflare.restorePublic")(function* (session: PloyzSession) {
  const answered = yield* session.setIngressDoors(PUBLIC).pipe(Effect.result);
  const gap = answered._tag === "Failure" ? "Your servers didn't answer." : doorsGap(answered.success, PUBLIC);
  return gap === null ? null : `Your servers still hold the tunnel door. ${gap}`;
});

/**
 * Set Cloudflare up for the Cluster, once, with a token the user pasted for it. Order matters: every permission is
 * checked before anything else, then the Cluster is reserved (`setting_up`) under the lock a switch takes, so no switch
 * starts until setup ends and setup never starts over one; the Servers answer and the zone's wildcard is free before
 * anything is created, the tunnel exists before the Cluster is asked to run it, both Doors are open and answering
 * before the wildcard record points names at it, and the row becomes `set_up` last, under the same lock. The API token
 * is `Redacted` throughout, used for these calls and then dropped; the tunnel's own token goes `Redacted` to the
 * Cluster, which seals it. Every way out after the reservation undoes what was done, puts the row back as it was, and
 * names what was kept.
 */
export const setUpCloudflare = Effect.fn("Cloudflare.setUp")(function* (
  organizationId: string,
  input: { readonly token: Redacted.Redacted<string>; readonly zone: string },
  http: CloudflareHttp = realCloudflare,
) {
  const database = yield* Database;
  const { drizzle } = database;
  const early = yield* setupProblem(organizationId);
  if (early !== null) return yield* new Conflict({ userFacing: true, message: early });
  const zones = yield* listZones(http, input.token).pipe(untouched);
  const zoneTrouble = zoneProblem(input.zone, zones.map((zone) => zone.name));
  if (zoneTrouble !== null) return yield* refuse(zoneTrouble);
  const zone = zones.find((candidate) => candidate.name === normalizeZone(input.zone));
  if (zone === undefined) return yield* refuse(`${normalizeZone(input.zone)} isn't a zone in this Cloudflare account.`);
  yield* checkEditPermissions(http, input.token, zone).pipe(untouched);
  const [org] = yield* drizzle.select({ slug: organization.slug }).from(organization).where(eq(organization.id, organizationId));
  const tunnelName = `ployz-${org?.slug ?? organizationId.slice(0, 8)}`;
  const name = `*.${zone.name}`;

  // The reservation: the row as it was, to put back, and the moment written as `updated_at`, which names this setup.
  const reservedAt = new Date();
  const reservation = { zone: zone.name, tunnelId: null, tunnelName, state: "setting_up", updatedAt: reservedAt } as const;
  const prior = yield* database.transaction(Effect.gen(function* () {
    yield* lockIngressRows(organizationId);
    const problem = yield* setupProblem(organizationId);
    if (problem !== null) return yield* new Conflict({ userFacing: true, message: problem });
    const { drizzle } = yield* Database;
    const [was] = yield* drizzle.select().from(organizationCloudflareIngress).where(eq(organizationCloudflareIngress.organizationId, organizationId));
    yield* drizzle.insert(organizationCloudflareIngress).values({ organizationId, ...reservation })
      .onConflictDoUpdate({ target: organizationCloudflareIngress.organizationId, set: reservation });
    return was ?? null;
  }));
  const reserved = and(
    eq(organizationCloudflareIngress.organizationId, organizationId),
    eq(organizationCloudflareIngress.state, "setting_up"),
    eq(organizationCloudflareIngress.updatedAt, reservedAt),
  );
  const putBack = database.transaction(Effect.gen(function* () {
    yield* lockIngressRows(organizationId);
    const { drizzle } = yield* Database;
    if (prior === null) return yield* drizzle.delete(organizationCloudflareIngress).where(reserved);
    const { zone, tunnelId, tunnelName, state, updatedAt } = prior;
    yield* drizzle.update(organizationCloudflareIngress).set({ zone, tunnelId, tunnelName, state, updatedAt }).where(reserved);
  }));

  let session: PloyzSession | null = null;
  let tunnel: { readonly id: string; readonly made: boolean } | null = null;
  let recordId: string | null = null;
  let closed = false;
  /**
   * Put back what the steps below changed: the Doors first, so no connector holds the tunnel, then the record and the
   * tunnel this setup made, and the row last, so the Cluster stays reserved until nothing of this setup is left for
   * another to pick up. What is left for the user, in words; a success line is true when it prints.
   */
  const cleanUp = Effect.gen(function* () {
    const doorsLeft = session === null ? null : yield* restorePublic(session);
    // The Engine sets no empty Door set, so the only way to close the tunnel this setup opened is back to the ports. Any
    // Server that took them now publishes 80 and 443, even when another didn't answer.
    const opened = !closed || session === null ? null
      : doorsLeft === null ? "Your servers now take web traffic on ports 80 and 443." : "Some servers may now take web traffic on ports 80 and 443.";
    const recordGone = recordId === null ? true : yield* deleteRecord(http, input.token, zone.id, recordId);
    const tunnelGone = tunnel === null || !tunnel.made ? true : yield* deleteTunnel(http, input.token, zone.accountId, tunnel.id);
    const kept = [recordGone ? null : name, tunnelGone ? null : `the tunnel ${tunnelName}`].filter((item) => item !== null);
    const left = [doorsLeft, opened, kept.length === 0 ? null : `Delete ${kept.join(" and ")} in Cloudflare.`].filter((item) => item !== null);
    if (left.length === 0) return "Nothing was changed.";
    yield* Effect.logWarning(`A failed Cloudflare setup left: ${left.join(" ")}`);
    return left.join(" ");
  }).pipe(Effect.timeoutOrElse({
    duration: SETUP_RUNNING_LIMIT_MS,
    orElse: () => Effect.succeed(`Check your servers' doors, and the tunnel ${tunnelName} and ${name} in Cloudflare.`),
  }));
  const undo = cleanUp.pipe(Effect.tap(() => putBack));

  const finish = Effect.gen(function* () {
    const opened = yield* (yield* OrganizationRuntime).open(organizationId);
    if (opened.status !== "connected") return yield* refuse("Your servers aren't answering. Try again once they are.");
    // A Cluster founded closed holds no Public Door to hand over, so only the tunnel opens and the ports stay shut.
    const founding = yield* closedFounding(organizationId);
    if (founding.kind === "unknown") return yield* refuse("Your servers aren't answering yet. Try again once they all do.");
    closed = founding.kind === "closed";
    const doors = closed ? TUNNEL : BOTH;
    const found = yield* findTunnel(http, input.token, zone.accountId, tunnelName);
    const there = yield* findRecords(http, input.token, zone.id, name);
    if (there.some((row) => found === null || row.content !== wildcardRecord(zone.name, found).content)) {
      return yield* refuse(`${name} already points somewhere else in Cloudflare. Remove that record or pick another zone.`);
    }
    const made = found === null ? yield* createTunnel(http, input.token, zone.accountId, tunnelName) : { id: found, token: null };
    tunnel = { id: made.id, made: found === null };
    const token = made.token ?? (yield* tunnelToken(http, input.token, zone.accountId, made.id));
    session = opened.connected;
    const answered = yield* session.setIngressDoors(doors, token).pipe(Effect.result);
    if (answered._tag === "Failure") return yield* refuse("Your servers didn't take the tunnel.");
    const gap = doorsGap(answered.success, doors);
    if (gap !== null) return yield* refuse(gap);

    if (there.length === 0) recordId = yield* createRecord(http, input.token, zone.id, wildcardRecord(zone.name, tunnel.id));

    // With nothing published to move, the zone is the Cluster Domain's source at once, as a switch would leave it.
    const state = founding.kind === "closed" && founding.hostnames.length === 0 ? "active" : "set_up";
    const row = { zone: zone.name, tunnelId: tunnel.id, tunnelName, state } as const;
    return yield* database.transaction(Effect.gen(function* () {
      yield* lockIngressRows(organizationId);
      const { drizzle } = yield* Database;
      const [written] = yield* drizzle.update(organizationCloudflareIngress).set({ ...row, updatedAt: new Date() }).where(reserved)
        .returning({ organizationId: organizationCloudflareIngress.organizationId });
      if (written === undefined) return yield* new Conflict({ userFacing: true, message: "Another setup took over this one." });
      return row;
    }));
  }).pipe(Effect.timeoutOrElse({ duration: SETUP_RUNNING_LIMIT_MS, orElse: () => Effect.fail(refuse("Setup didn't finish in time.")) }));

  const ended = yield* finish.pipe(words, Effect.exit);
  if (Exit.isSuccess(ended)) return ended.value;
  const left = yield* undo;
  const failure = Cause.findErrorOption(ended.cause);
  if (failure._tag === "Some" && failure.value instanceof Conflict) return yield* new Conflict({ userFacing: true, message: `${failure.value.message} ${left}` });
  if (failure._tag === "Some" && failure.value instanceof Validation) return yield* refuse(`${failure.value.message} ${left}`);
  if (Cause.hasInterruptsOnly(ended.cause)) return yield* Effect.failCause(ended.cause);
  // Whatever ended it, the undo above ran, so what it left, ports opened included, is the user's to know.
  yield* Effect.logError("Cloudflare setup failed", ended.cause);
  return yield* refuse(`Setup didn't finish. ${left}`);
}, Effect.scoped);
