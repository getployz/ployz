import "@tanstack/react-start/server-only";
import type { AppliedVolume, MachineId } from "@ployz/sdk";
import { eq } from "drizzle-orm";
import { Effect, Option, Schema } from "effect";
import type { EncryptedSecretValue } from "#/db/tables";
import type { Gate, OperationAsked } from "#/modules/approvals/approvals.server";
import { cloudStore, storeTry } from "#/modules/config-store/store-sdk.server";
import type { StoreRefusal, StoreResult } from "#/modules/config-store/store.contract";
import type { Actor } from "#/modules/identity/actor";
import { decryptPairingSecret, foundingClaimFresh, SERVER_REACH_TIMEOUT_MS } from "#/modules/machines/connections.server";
import { RemovalEndpoints } from "#/modules/machines/pairing-removal";
import { forgetCluster, seeCluster } from "#/modules/machines/pairing-removal.server";
import { enrollmentAllocation, organizationMachine } from "#/modules/machines/tables";
import { requireAccess, requireAccessById } from "#/modules/organization/access.server";
import { Ployz } from "#/modules/runtime/ployz.server";
import { Database } from "#/server/database.server";

/**
 * One Server as Cloud found it when it tried: it didn't answer in time, or Cloud holds no connection to try (a founder
 * that never published one). One that answered refuses the whole forget, so no caller sees it.
 */
type ObservedServer = { id: string; name: string; reach: "didnt_answer" | "no_connection" };
type ForgetServersCheck = { servers: ObservedServer[]; volumes: AppliedVolume[] };
type ServersForgotten = ForgetServersCheck & { cancelled: string[] };

/** Who forgets: someone who may operate the Organization; only those who may manage it get past the check. */
type Forgetter = Actor & { readonly organizationId: string };

/** The dashboard's forgetter: `actor`, in the Organization named by `organizationSlug`, which they must reach. */
export const forgetterFor = Effect.fn("ForgetServers.forgetterFor")(function* (actor: Actor, organizationSlug: string) {
  const { organization } = yield* requireAccess(actor, organizationSlug, "servers:read");
  return { userId: actor.userId, credential: actor.credential, organizationId: organization.id } satisfies Forgetter;
});

const refused = (refusal: StoreRefusal) => ({ ok: false as const, refusal });

const mayManage = (forgetter: Forgetter) => requireAccessById(forgetter, forgetter.organizationId, "servers:remove").pipe(
  Effect.as(null),
  Effect.catchTag("Forbidden", (forbidden) => Effect.succeed(refused({ code: "forbidden", message: forbidden.message, details: null }))),
);

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
  const servers = new Map<MachineId, EncryptedSecretValue | null>(machines.map((machine) => [machine.machineId, machine.encryptedCapability]));
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

/** A Server that just answered says its name at once; one that doesn't still counts as answering. */
const NAME_TIMEOUT_MS = 2_000;

/** One connection attempt to each known Server, all at once, each bounded by the timeout. One that answers says its name. */
const reachServers = Effect.fn("ForgetServers.reach")(function* (known: ReadonlyArray<{ id: MachineId; name: string; management: string | null }>) {
  const ployz = yield* Ployz;
  type Reach = "answered" | ObservedServer["reach"];
  return yield* Effect.forEach(known, ({ id, name, management }): Effect.Effect<{ id: MachineId; name: string; reach: Reach }> => {
    if (management === null) return Effect.succeed({ id, name, reach: "no_connection" as const });
    const connection = { machine_id: id, management };
    return Effect.scoped(Effect.gen(function* () {
      const session = yield* ployz.connect({ connections: [connection], timeoutMs: SERVER_REACH_TIMEOUT_MS });
      const details = yield* session.inspect().pipe(Effect.timeout(NAME_TIMEOUT_MS), Effect.option);
      return Option.match(details, { onNone: () => name, onSome: ({ machine }) => machine?.name ?? name });
    })).pipe(
      Effect.map((answered) => ({ id, name: answered, reach: "answered" as const })),
      Effect.orElseSucceed(() => ({ id, name, reach: "didnt_answer" as const })),
    );
  }, { concurrency: "unbounded" });
});

/** Tries every Server; refuses while one is still joining or any answers, naming those that did. */
const check = Effect.fn("ForgetServers.checkReach")(function* (forgetter: Forgetter) {
  const denied = yield* mayManage(forgetter);
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
 * Forgetting Organization `organization`'s Servers as a human approves it, by its slug, which `ployz server forget`
 * confirms: each Server leaves, and each Volume's data on them is lost.
 */
export const forgetServersAsked = (organization: { id: string; slug: string }, { servers, volumes }: ForgetServersCheck): OperationAsked => {
  const lost = volumes.map(({ project, environment, volume }) => `${project}/${environment}/${volume}`).sort();
  return {
    subject: `servers:${organization.id}`,
    verb: "forget",
    name: organization.slug,
    preview: { servers: servers.map((server) => server.id).sort(), volumes: lost },
    effects: [
      ...servers.map((server) => ({ kind: "removes_server" as const, name: server.name, node: server.id, path: `servers/${server.id}` })),
      ...lost.map((name) => ({ kind: "deletes_volume" as const, name, node: name, path: `volumes/${name}` })),
    ],
  };
};

/**
 * Forget Servers: the user says the Organization's Servers were deleted. Cloud tries each again and, when none answers
 * and nothing changed since, forgets its Cluster (`forgetCluster`), once `gate` lets it. Cloud does this on its own in
 * one case: a lapsed anonymous sandbox whose Servers every probe for an hour since the last answer failed to reach.
 */
export const forgetServers = Effect.fn("ForgetServers.forget")(function* (
  forgetter: Forgetter,
  gate?: (checked: ForgetServersCheck) => ReturnType<Gate>,
) {
  const { result: checked, seen } = yield* check(forgetter);
  if (!checked.ok || seen === null) return checked;
  if (gate !== undefined) {
    const gated = yield* gate(checked.value);
    if (!gated.ok) return refused(gated.refusal);
  }
  const forgotten = yield* forgetCluster(forgetter.organizationId, seen);
  if (forgotten.kind === "changed") {
    return refused({ code: "conflict", message: "Your Servers changed while Cloud tried them. Try again.", details: null });
  }
  const result: StoreResult<ServersForgotten> = { ok: true, value: { ...checked.value, cancelled: forgotten.cancelled } };
  return result;
});
