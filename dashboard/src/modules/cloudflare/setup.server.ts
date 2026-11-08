import "@tanstack/react-start/server-only";
import { eq } from "drizzle-orm";
import { Effect, Redacted } from "effect";
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

/** The names of the zones the token can see, so the page can offer them. Nothing is changed. */
export const listCloudflareZoneNames = Effect.fn("Cloudflare.zoneNames")(function* (token: Redacted.Redacted<string>, http: CloudflareHttp = realCloudflare) {
  const zones = yield* listZones(http, token).pipe(words);
  return zones.map((zone) => zone.name).sort();
});

/**
 * Set Cloudflare up for the Cluster, once, with a token the user pasted for it. Order matters: every permission is
 * checked before anything is created, the tunnel exists before the Cluster is asked to run it, both Doors are open and
 * answering before the wildcard record points names at it, and the record is the last thing written. The API token is
 * `Redacted` throughout, used for these calls and then dropped; the tunnel's own token goes straight to the Cluster,
 * which seals it. A failure after the tunnel was created here deletes it again, so nothing is left behind.
 */
export const setUpCloudflare = Effect.fn("Cloudflare.setUp")(function* (
  organizationId: string,
  input: { readonly token: Redacted.Redacted<string>; readonly zone: string },
  http: CloudflareHttp = realCloudflare,
) {
  const { drizzle } = yield* Database;
  const existing = yield* loadCloudflareSetup(organizationId);
  if (existing?.state === "active") return yield* new Conflict({ userFacing: true, message: "This Cluster already uses Cloudflare." });
  const zones = yield* listZones(http, input.token).pipe(words);
  const problem = zoneProblem(input.zone, zones.map((zone) => zone.name));
  if (problem !== null) return yield* refuse(problem);
  const zone = zones.find((candidate) => candidate.name === normalizeZone(input.zone));
  if (zone === undefined) return yield* refuse(`${normalizeZone(input.zone)} isn't a zone in this Cloudflare account.`);
  yield* checkEditPermissions(http, input.token, zone).pipe(words);

  const [org] = yield* drizzle.select({ slug: organization.slug }).from(organization).where(eq(organization.id, organizationId));
  const tunnelName = `ployz-${org?.slug ?? organizationId.slice(0, 8)}`;

  const found = yield* findTunnel(http, input.token, zone.accountId, tunnelName).pipe(words);
  const tunnel = found === null
    ? yield* createTunnel(http, input.token, zone.accountId, tunnelName).pipe(words)
    : { id: found, token: yield* tunnelToken(http, input.token, zone.accountId, found).pipe(words) };

  const undo = found === null
    ? deleteTunnel(http, input.token, zone.accountId, tunnel.id).pipe(Effect.ignoreCause({ log: true, message: "Cloudflare tunnel not deleted after a failed setup" }))
    : Effect.void;
  const stop = (message: string) => Effect.andThen(undo, refuse(message));

  const session = yield* (yield* OrganizationRuntime).open(organizationId);
  if (session.status !== "connected") return yield* stop("Your servers aren't answering. Try again once they are. Nothing was changed.");
  const answered = yield* session.connected.setIngressDoors(BOTH, Redacted.value(tunnel.token)).pipe(Effect.result);
  if (answered._tag === "Failure") {
    return yield* stop("Your servers didn't take the tunnel. Nothing was changed.");
  }
  const gap = doorsGap(answered.success, BOTH);
  if (gap !== null) {
    yield* session.connected.setIngressDoors(["public"]).pipe(Effect.ignoreCause({ log: true, message: "Public door not restored after a failed setup" }));
    return yield* stop(`${gap} Nothing was changed.`);
  }

  const name = `*.${zone.name}`;
  const wanted = wildcardRecord(zone.name, tunnel.id);
  const there = yield* findRecords(http, input.token, zone.id, name).pipe(words);
  if (there.length === 0) {
    yield* createRecord(http, input.token, zone.id, wanted).pipe(words, Effect.tapError(() => undo));
  } else if (!there.every((row) => row.content === wanted.content)) {
    return yield* stop(`${name} already points somewhere else in Cloudflare. Remove that record or pick another zone. Nothing was changed.`);
  }

  const now = new Date();
  yield* drizzle.insert(organizationCloudflareIngress).values({
    organizationId, zone: zone.name, tunnelId: tunnel.id, tunnelName, state: "set_up",
  }).onConflictDoUpdate({
    target: organizationCloudflareIngress.organizationId,
    set: { zone: zone.name, tunnelId: tunnel.id, tunnelName, state: "set_up", updatedAt: now },
  });
  return { zone: zone.name, tunnelName, tunnelId: tunnel.id, state: "set_up" } as const;
}, Effect.scoped);
