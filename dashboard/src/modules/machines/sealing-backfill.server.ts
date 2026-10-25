import "@tanstack/react-start/server-only";
import type { BackfillSealingKeyReport, BackfillSealingKeyRequest, MachineId } from "@ployz/sdk";
import { eq } from "drizzle-orm";
import { Data, Effect } from "effect";
import { decryptPairingSecret } from "#/modules/machines/connections.server";
import { hashEnrollmentToken } from "#/modules/machines/enrollment.server";
import {
  chooseSealingKeyGenerator,
  forgetSealingKeyGenerator,
  readOrganizationSealingKey,
  readSealingKeyGenerator,
  recordSealingPublicKey,
} from "#/modules/machines/sealing-key.server";
import { readSealingPublicKey } from "#/modules/machines/sealing-key-read.server";
import { clusterRead, OrganizationRuntime, withClusterBudget } from "#/modules/runtime/organization-runtime.server";
import { organizationPairing } from "#/modules/runtime/tables";
import { Database } from "#/server/database.server";

const BACKFILL_CLUSTER_BUDGET_MS = 60_000;

export class SealingBackfillUnreachable extends Data.TaggedError("SealingBackfillUnreachable")<{
  readonly cause: unknown;
}> {}

export type SealingBackfillTarget = {
  readonly clusterKey: string;
  readonly expected: BackfillSealingKeyRequest["expected"];
  readonly generator: MachineId | null;
};

export type SealingBackfillNext =
  | { readonly next: "rerun"; readonly generator: MachineId }
  | { readonly next: "stop"; readonly result: BackfillSealingKeyReport["result"]; readonly awaitsOperator: boolean };

export const loadSealingBackfillTarget = Effect.fn("SealingBackfill.loadTarget")(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  const [pairing] = yield* drizzle.select({
    encryptedPairingSecret: organizationPairing.encryptedPairingSecret,
    founderMachineId: organizationPairing.founderMachineId,
    removalStartedAt: organizationPairing.removalStartedAt,
  }).from(organizationPairing).where(eq(organizationPairing.organizationId, organizationId));
  if (pairing === undefined || pairing.founderMachineId === null || pairing.removalStartedAt !== null) return null;
  const clusterKey = hashEnrollmentToken(yield* decryptPairingSecret(pairing.encryptedPairingSecret));
  const expected = (yield* readSealingPublicKey(organizationId, clusterKey))
    ?? (yield* readOrganizationSealingKey(organizationId))?.publicKey;
  return {
    clusterKey,
    expected: expected ?? null,
    generator: (yield* readSealingKeyGenerator(organizationId, clusterKey)) ?? null,
  } satisfies SealingBackfillTarget;
});

export const backfillClusterSealingKey = Effect.fn("SealingBackfill.backfill")(function* (
  organizationId: string, request: BackfillSealingKeyRequest,
) {
  const session = yield* (yield* OrganizationRuntime).open(organizationId);
  if (session.status === "no_connection") return null;
  if (session.status !== "connected") return yield* new SealingBackfillUnreachable({ cause: session.status });
  return yield* session.connected.backfillSealingKey(request).pipe(clusterRead);
}, Effect.scoped, withClusterBudget(BACKFILL_CLUSTER_BUDGET_MS));

export const handleSealingBackfillReport = Effect.fn("SealingBackfill.handleReport")(function* (
  organizationId: string, clusterKey: string, report: BackfillSealingKeyReport,
) {
  const stop = (awaitsOperator = false): SealingBackfillNext => ({ next: "stop", result: report.result, awaitsOperator });
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
