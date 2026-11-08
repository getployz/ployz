import "@tanstack/react-start/server-only";
import { eq } from "drizzle-orm";
import { Cause, Effect, Exit, type Redacted } from "effect";
import {
  CloudflareError,
  CloudflareHttp,
  checkEditPermissions,
  createRecord,
  createTunnel,
  deleteTunnel,
  findRecords,
  findTunnel,
  listZones,
  refusalWords,
  tunnelToken,
} from "#/modules/cloudflare/api.server";
import { loadCloudflareSetup } from "#/modules/cloudflare/source.server";
import { normalizeZone, wildcardRecord, zoneProblem } from "#/modules/cloudflare/zone";
import { doorsGap } from "#/modules/ingress/doors";
import { activeDoorsAttempt, lockIngressRows } from "#/modules/ingress/doors.lock.server";
import { organizationCloudflareIngress } from "#/modules/ingress/tables";
import { organization } from "#/modules/organization/tables";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { Database } from "#/server/database.server";
import { Conflict, Validation } from "#/server/public-error";

/** The Cloudflare Cloud calls through; tests give it a mock of the API. */
export const realCloudflare = new CloudflareHttp({ fetch: (input, init) => fetch(input, init) });

const BOTH = ["public", "tunnel"] as const;

const refuse = (message: string) => new Validation({ message, userFacing: true });

/** A Cloudflare refusal as the words a user reads, so the token and Cloudflare's detail stay out of the reply. */
const words = <A, E extends { readonly _tag: string }, R>(effect: Effect.Effect<A, E, R>) =>
  effect.pipe(Effect.catchIf(
    (error): error is Extract<E, CloudflareError> => error instanceof CloudflareError,
    (error) => Effect.fail(refuse(refusalWords(error))),
  ));

/** `words`, for the steps before anything is created: a refusal there leaves everything as it was. */
const untouched = <A, E extends { readonly _tag: string }, R>(effect: Effect.Effect<A, E, R>) =>
  effect.pipe(words, Effect.mapError((error) => error instanceof Validation ? refuse(`${error.message} Nothing was changed.`) : error));

/** The names of the zones the token can see, so the page can offer them. Nothing is changed. */
export const listCloudflareZoneNames = Effect.fn("Cloudflare.zoneNames")(function* (token: Redacted.Redacted<string>, http: CloudflareHttp = realCloudflare) {
  const zones = yield* listZones(http, token).pipe(words);
  return zones.map((zone) => zone.name).sort();
});

/** Why the Cluster can't be set up now: it already uses Cloudflare, or a switch is moving its Doors. */
const setupProblem = Effect.fn("Cloudflare.setupProblem")(function* (organizationId: string) {
  const existing = yield* loadCloudflareSetup(organizationId);
  if (existing?.state === "active") return "This Cluster already uses Cloudflare.";
  const switching = yield* activeDoorsAttempt(organizationId);
  return switching === null ? null : "A switch is under way. Wait for it to finish, then set up.";
});

/**
 * Set Cloudflare up for the Cluster, once, with a token the user pasted for it. Order matters: every permission is
 * checked and the Servers answer before anything is created, the tunnel exists before the Cluster is asked to run it,
 * both Doors are open and answering before the wildcard record points names at it, and the row is the last thing
 * written, under the lock a switch takes, so neither lands over the other. The API token is `Redacted` throughout,
 * used for these calls and then dropped; the tunnel's own token goes `Redacted` to the Cluster, which seals it. Every
 * way out after the tunnel exists undoes what was done, and says so when Cloudflare kept the tunnel.
 */
export const setUpCloudflare = Effect.fn("Cloudflare.setUp")(function* (
  organizationId: string,
  input: { readonly token: Redacted.Redacted<string>; readonly zone: string },
  http: CloudflareHttp = realCloudflare,
) {
  const database = yield* Database;
  const { drizzle } = database;
  const problem = yield* setupProblem(organizationId);
  if (problem !== null) return yield* new Conflict({ userFacing: true, message: problem });
  const zones = yield* listZones(http, input.token).pipe(untouched);
  const zoneTrouble = zoneProblem(input.zone, zones.map((zone) => zone.name));
  if (zoneTrouble !== null) return yield* refuse(zoneTrouble);
  const zone = zones.find((candidate) => candidate.name === normalizeZone(input.zone));
  if (zone === undefined) return yield* refuse(`${normalizeZone(input.zone)} isn't a zone in this Cloudflare account.`);
  yield* checkEditPermissions(http, input.token, zone).pipe(untouched);
  const session = yield* (yield* OrganizationRuntime).open(organizationId);
  if (session.status !== "connected") return yield* refuse("Your servers aren't answering. Try again once they are. Nothing was changed.");

  const [org] = yield* drizzle.select({ slug: organization.slug }).from(organization).where(eq(organization.id, organizationId));
  const tunnelName = `ployz-${org?.slug ?? organizationId.slice(0, 8)}`;
  const found = yield* findTunnel(http, input.token, zone.accountId, tunnelName).pipe(untouched);
  const tunnel = found === null
    ? { ...yield* createTunnel(http, input.token, zone.accountId, tunnelName).pipe(untouched), made: true }
    : { id: found, token: null, made: false };

  let doorsAsked = false;
  /** Put back what the steps below changed: the Doors, then the tunnel this setup made. What is left for the user, in words. */
  const undo = Effect.gen(function* () {
    if (doorsAsked) yield* session.connected.setIngressDoors(["public"]).pipe(Effect.ignoreCause({ log: true, message: "Public door not restored after a failed setup" }));
    const gone = tunnel.made ? yield* deleteTunnel(http, input.token, zone.accountId, tunnel.id) : true;
    if (!gone) yield* Effect.logWarning(`Cloudflare kept tunnel ${tunnel.id} after a failed setup`);
    return gone ? "Nothing was changed." : `Delete the tunnel ${tunnelName} in Cloudflare.`;
  });

  const finish = Effect.gen(function* () {
    const token = tunnel.token ?? (yield* tunnelToken(http, input.token, zone.accountId, tunnel.id));
    doorsAsked = true;
    const answered = yield* session.connected.setIngressDoors(BOTH, token).pipe(Effect.result);
    if (answered._tag === "Failure") return yield* refuse("Your servers didn't take the tunnel.");
    const gap = doorsGap(answered.success, BOTH);
    if (gap !== null) return yield* refuse(gap);

    const name = `*.${zone.name}`;
    const wanted = wildcardRecord(zone.name, tunnel.id);
    const there = yield* findRecords(http, input.token, zone.id, name);
    if (there.length === 0) {
      yield* createRecord(http, input.token, zone.id, wanted);
    } else if (!there.every((row) => row.content === wanted.content)) {
      return yield* refuse(`${name} already points somewhere else in Cloudflare. Remove that record or pick another zone.`);
    }

    return yield* database.transaction(Effect.gen(function* () {
      yield* lockIngressRows(organizationId);
      const late = yield* setupProblem(organizationId);
      if (late !== null) return yield* new Conflict({ userFacing: true, message: late });
      const { drizzle } = yield* Database;
      const row = { zone: zone.name, tunnelId: tunnel.id, tunnelName, state: "set_up" } as const;
      yield* drizzle.insert(organizationCloudflareIngress).values({ organizationId, ...row })
        .onConflictDoUpdate({ target: organizationCloudflareIngress.organizationId, set: { ...row, updatedAt: new Date() } });
      return row;
    }));
  });

  const ended = yield* finish.pipe(words, Effect.exit);
  if (Exit.isSuccess(ended)) return ended.value;
  const left = yield* undo;
  const failure = Cause.findErrorOption(ended.cause);
  if (failure._tag === "Some" && failure.value instanceof Conflict) return yield* new Conflict({ userFacing: true, message: `${failure.value.message} ${left}` });
  if (failure._tag === "Some" && failure.value instanceof Validation) return yield* refuse(`${failure.value.message} ${left}`);
  return yield* Effect.failCause(ended.cause);
}, Effect.scoped);
