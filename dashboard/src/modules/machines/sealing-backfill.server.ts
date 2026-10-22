import "@tanstack/react-start/server-only";
import type { BackfillSealingKeyReport, BackfillSealingKeyRequest, MachineId } from "@ployz/sdk";
import { eq } from "drizzle-orm";
import { Data, Effect } from "effect";
import { decryptPairingSecret } from "#/modules/machines/connections.server";
import { hashEnrollmentToken } from "#/modules/machines/enrollment.server";
import {
  chooseSealingKeyGenerator,
  forgetSealingKeyGenerator,
  readSealingKeyGenerator,
  readSealingPublicKey,
  recordSealingPublicKey,
} from "#/modules/machines/sealing-key.server";
import { clusterRead, OrganizationRuntime, withClusterBudget } from "#/modules/runtime/organization-runtime.server";
import { organizationPairing } from "#/modules/runtime/tables";
import { Database } from "#/server/database.server";

/** How long one backfill may spend in the Cluster: each Machine is asked in turn, and a generation is one more round. */
const BACKFILL_CLUSTER_BUDGET_MS = 60_000;

export class SealingBackfillUnreachable extends Data.TaggedError("SealingBackfillUnreachable")<{
  readonly cause: unknown;
}> {}

/** The pairing generation a backfill runs for, and what Cloud kept of its Sealing Key. */
export type SealingBackfillTarget = {
  readonly clusterKey: string;
  readonly expected: BackfillSealingKeyRequest["expected"];
  readonly generator: MachineId | null;
};

/** What a run does after one report. */
export type SealingBackfillNext =
  | { readonly next: "rerun"; readonly generator: MachineId }
  | { readonly next: "stop"; readonly result: BackfillSealingKeyReport["result"]; readonly terminal: boolean };

/** The founded, unremoved Cluster's pairing generation and its recorded key and generator; none without one. */
export const loadSealingBackfillTarget = Effect.fn("SealingBackfill.loadTarget")(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  const [pairing] = yield* drizzle.select({
    encryptedPairingSecret: organizationPairing.encryptedPairingSecret,
    founderMachineId: organizationPairing.founderMachineId,
    removalStartedAt: organizationPairing.removalStartedAt,
  }).from(organizationPairing).where(eq(organizationPairing.organizationId, organizationId));
  if (pairing === undefined || pairing.founderMachineId === null || pairing.removalStartedAt !== null) return null;
  const clusterKey = hashEnrollmentToken(yield* decryptPairingSecret(pairing.encryptedPairingSecret));
  return {
    clusterKey,
    expected: (yield* readSealingPublicKey(organizationId, clusterKey)) ?? null,
    generator: (yield* readSealingKeyGenerator(organizationId, clusterKey)) ?? null,
  } satisfies SealingBackfillTarget;
});

/** Asks the Cluster to bring each Machine to its Sealing Key; none when no Cluster answers for the Organization. */
export const backfillClusterSealingKey = Effect.fn("SealingBackfill.backfill")(function* (
  organizationId: string, request: BackfillSealingKeyRequest,
) {
  const session = yield* (yield* OrganizationRuntime).open(organizationId);
  if (session.status === "no_connection") return null;
  if (session.status !== "connected") return yield* new SealingBackfillUnreachable({ cause: session.status });
  return yield* session.connected.backfillSealingKey(request).pipe(clusterRead);
}, Effect.scoped, withClusterBudget(BACKFILL_CLUSTER_BUDGET_MS));

/**
 * Acts on one report: records a distributed key, chooses or forgets the generator, and logs the rest. `rerun` asks for
 * one more backfill with the chosen generator; a stop marked `terminal` waits on an operator, not on another run.
 */
export const handleSealingBackfillReport = Effect.fn("SealingBackfill.handleReport")(function* (
  organizationId: string, clusterKey: string, report: BackfillSealingKeyReport,
) {
  const stop = (terminal = false): SealingBackfillNext => ({ next: "stop", result: report.result, terminal });
  switch (report.result) {
    case "distributed": {
      yield* recordSealingPublicKey(organizationId, clusterKey, report.public_key);
      for (const machine of report.machines) {
        if (machine.result === "failed" || machine.result === "conflict" || machine.result === "unknown") {
          yield* Effect.logWarning("A Server did not take its Cluster's Sealing Key.").pipe(Effect.annotateLogs({ machine }));
        }
      }
      return stop();
    }
    case "needs_generator": {
      const generator = yield* chooseSealingKeyGenerator(organizationId, clusterKey, report.candidate);
      return { next: "rerun", generator } satisfies SealingBackfillNext;
    }
    case "generator_gone":
      yield* forgetSealingKeyGenerator(organizationId, clusterKey, report.generator);
      return stop();
    case "conflicting":
    case "generation_failed":
      yield* Effect.logError("The Cluster's Sealing Key could not be backfilled.").pipe(Effect.annotateLogs({ report }));
      return stop();
    case "awaits_recovery":
    case "no_holder":
      yield* Effect.logError("The Cluster's Sealing Key awaits recovery.").pipe(Effect.annotateLogs({ report }));
      return stop(true);
    case "no_candidate":
    case "waiting":
      yield* Effect.logInfo("The Cluster's Sealing Key backfill is waiting on its Servers.").pipe(Effect.annotateLogs({ report }));
      return stop();
  }
}, (effect, organizationId) => effect.pipe(Effect.annotateLogs({ organizationId })));
