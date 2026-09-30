import "@tanstack/react-start/server-only";
import type { AppliedVolume, MachineId } from "@ployz/sdk";
import { and, eq, inArray } from "drizzle-orm";
import { Effect, Schema } from "effect";
import type { EncryptedSecretValue } from "#/db/tables";
import { cloudStore, storeTry } from "#/modules/config-store/store-sdk.server";
import type { StoreRefusal, StoreResult } from "#/modules/config-store/store.contract";
import type { Actor } from "#/modules/identity/actor";
import { member } from "#/modules/identity/tables";
import { decryptPairingSecret, foundingClaimFresh, SERVER_REACH_TIMEOUT_MS } from "#/modules/machines/connections.server";
import { RemovalEndpoints } from "#/modules/machines/pairing-removal";
import { forgetCluster, seeCluster } from "#/modules/machines/pairing-removal.server";
import { enrollmentAllocation, organizationMachine } from "#/modules/machines/tables";
import { requireOrganizationForMember } from "#/modules/organization/organization-state.server";
import { Ployz } from "#/modules/runtime/ployz.server";
import { Database } from "#/server/database.server";

/**
 * One Server as Cloud found it when it tried: it didn't answer in time, or Cloud holds no connection to try (a founder
 * that never published one). One that answered refuses the whole forget, so no caller sees it.
 */
type ObservedServer = { id: string; name: string; reach: "didnt_answer" | "no_connection" };
type ForgetServersCheck = { servers: ObservedServer[]; volumes: AppliedVolume[] };
type ServersForgotten = ForgetServersCheck & { cancelled: string[] };

/** Who forgets: a member of the Organization, which only an owner or admin may do. */
type Forgetter = { readonly userId: string; readonly organizationId: string };

/** The dashboard's forgetter: `actor`, in the Organization named by `organizationSlug`, which they must be a member of. */
export const forgetterFor = Effect.fn("ForgetServers.forgetterFor")(function* (actor: Actor, organizationSlug: string) {
  const organization = yield* requireOrganizationForMember(actor, organizationSlug);
  return { userId: actor.userId, organizationId: organization.id } satisfies Forgetter;
});

const refused = (refusal: StoreRefusal) => ({ ok: false as const, refusal });

const requireAdmin = Effect.fn("ForgetServers.requireAdmin")(function* ({ userId, organizationId }: Forgetter) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ role: member.role }).from(member).where(and(
    eq(member.userId, userId), eq(member.organizationId, organizationId), inArray(member.role, ["owner", "admin"]),
  )).limit(1);
  return row === undefined
    ? refused({ code: "forbidden", message: "Only an owner or admin of this Organization can forget its Servers.", details: null })
    : null;
});

/**
 * Every Server Cloud knows in the Organization's Cluster: its published connections, the Machines a stuck removal still
 * waits on, and the founder. Cloud holds no Server names but the ones enrollment assigned, so the rest show by ID.
 */
const knownServers = Effect.fn("ForgetServers.known")(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  const { pairing, seen } = yield* seeCluster(organizationId);
  const machines = yield* drizzle.select().from(organizationMachine).where(eq(organizationMachine.organizationId, organizationId));
  const allocations = yield* drizzle.select().from(enrollmentAllocation).where(eq(enrollmentAllocation.organizationId, organizationId));
  const names = new Map<string, string>(allocations.flatMap((allocation) => allocation.assignments.map((assignment) => [assignment.machine.id, assignment.machine.name])));
  const servers = new Map<string, EncryptedSecretValue | null>(machines.map((machine) => [machine.machineId, machine.encryptedCapability]));
  const endpoints = pairing?.removalEndpoints
    ? yield* Schema.decodeUnknownEffect(RemovalEndpoints)(pairing.removalEndpoints).pipe(Effect.orDie)
    : [];
  for (const endpoint of endpoints) {
    // A confirmed one let go of Cloud already.
    if (endpoint.status !== "confirmed" && !servers.has(endpoint.machineId)) {
      servers.set(endpoint.machineId, endpoint.status === "pending" ? endpoint.encryptedExpected : null);
    }
  }
  if (pairing && !servers.has(pairing.founderClaimMachineId)) servers.set(pairing.founderClaimMachineId, null);
  const known = yield* Effect.forEach([...servers], ([id, capability]) => Effect.gen(function* () {
    return { id, name: names.get(id) ?? id, management: capability === null ? null : yield* decryptPairingSecret(capability) };
  }));
  return { known, seen, joining: pairing !== undefined && foundingClaimFresh(pairing) };
});

/** One connection attempt to each known Server, all at once, each bounded by the timeout. */
const reachServers = Effect.fn("ForgetServers.reach")(function* (known: ReadonlyArray<{ id: string; name: string; management: string | null }>) {
  const ployz = yield* Ployz;
  type Reach = "answered" | ObservedServer["reach"];
  return yield* Effect.forEach(known, ({ id, name, management }): Effect.Effect<{ id: string; name: string; reach: Reach }> => {
    if (management === null) return Effect.succeed({ id, name, reach: "no_connection" as const });
    // SAFETY: the table constraint and the removal schema enforce the SDK Machine ID representation.
    const connection = { machine_id: id as MachineId, management };
    return Effect.scoped(ployz.connect({ connections: [connection], timeoutMs: SERVER_REACH_TIMEOUT_MS })).pipe(
      Effect.as("answered" as const),
      Effect.orElseSucceed(() => "didnt_answer" as const),
      Effect.map((reach) => ({ id, name, reach })),
    );
  }, { concurrency: "unbounded" });
});

/** Tries every Server; refuses while one is still joining or any answers, naming those that did. */
const check = Effect.fn("ForgetServers.checkReach")(function* (forgetter: Forgetter) {
  const denied = yield* requireAdmin(forgetter);
  if (denied) return { result: denied, seen: null };
  const { known, seen, joining } = yield* knownServers(forgetter.organizationId);
  if (joining) {
    return { result: refused({ code: "conflict", message: "A Server is still joining. Try again in a few minutes.", details: null }), seen };
  }
  const reached = yield* reachServers(known);
  const answered = reached.filter((server) => server.reach === "answered").map((server) => server.name);
  if (answered.length > 0) {
    const message = `Your Servers are still reachable (${answered.join(", ")}). Remove them from the Servers page or with \`ployz server rm\` instead.`;
    return { result: refused({ code: "conflict", message, details: { answered } }), seen };
  }
  const servers = reached.flatMap((server): ObservedServer[] => server.reach === "answered" ? [] : [{ ...server, reach: server.reach }]);
  const store = yield* cloudStore;
  const volumes = yield* storeTry(() => store.appliedVolumes(forgetter.organizationId));
  const result: StoreResult<ForgetServersCheck> = { ok: true, value: { servers, volumes } };
  return { result, seen };
});

/**
 * What Forget Servers would let go of, once Cloud tried every Server and none answered: each Server with what Cloud
 * observed of it, and every Volume a Deploy put on them. Refused while any Server answers (naming them) or one is
 * still joining, and `forbidden` for a member who isn't an owner or admin.
 */
export const checkForgetServers = Effect.fn("ForgetServers.check")(function* (forgetter: Forgetter) {
  return (yield* check(forgetter)).result;
});

/**
 * Forget Servers: the user says the Organization's Servers were deleted. Cloud tries each again and, when none answers
 * and nothing changed since, forgets its Cluster (`forgetCluster`). Cloud never does this on its own.
 */
export const forgetServers = Effect.fn("ForgetServers.forget")(function* (forgetter: Forgetter) {
  const { result: checked, seen } = yield* check(forgetter);
  if (!checked.ok || seen === null) return checked;
  const forgotten = yield* forgetCluster(forgetter.organizationId, seen);
  if (forgotten.kind === "changed") {
    return refused({ code: "conflict", message: "Your Servers changed while Cloud tried them. Try again.", details: null });
  }
  const result: StoreResult<ServersForgotten> = { ok: true, value: { ...checked.value, cancelled: forgotten.cancelled } };
  return result;
});
