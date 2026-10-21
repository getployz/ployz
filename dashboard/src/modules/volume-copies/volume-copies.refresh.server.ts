import "@tanstack/react-start/server-only";
import type { EnvironmentRef } from "@ployz/sdk";
import { and, eq } from "drizzle-orm";
import { Effect, Result } from "effect";
import type { StoreRefusal } from "#/modules/config-store/store.contract";
import { withClusterBudget } from "#/modules/runtime/organization-runtime.server";
import { type CopyAsk, failureLines, type MemberObserver, syncMirrors } from "#/modules/volume-placement/mirror-sync.server";
import { type BlockerReader, placementRefusal, readPlacementBlockers, UNANSWERED } from "#/modules/volume-placement/volume-placement.deploy.server";
import { volumeMirrorStatus } from "#/modules/volume-run/tables";
import type { MirrorStatusView } from "#/modules/volume-copies/volume-copies.placement";
import { unheardOf } from "#/modules/volume-run/volume-run";
import { askCopy, inspectVolumeCopies, observeVolumeMembers, resolveVolume } from "#/modules/volume-run/volume-run.server";
import { Database } from "#/server/database.server";

const silent = (names: readonly string[], failures: readonly string[] = []): StoreRefusal => ({
  code: "unanswered",
  message: [...failures, names.length === 1 ? `${names[0]} isn't answering` : `${names.join(", ")} aren't answering`].join("; "),
  details: null,
});

const lacking = (lacks: readonly string[], failures: readonly string[] = []): StoreRefusal => ({
  code: "unsupported",
  message: [...failures, ...lacks].join("; "),
  details: null,
});

const waiting = (failures: readonly string[] = []): StoreRefusal => ({
  code: "unanswered",
  message: [...failures, "Waiting for every server's report"].join("; "),
  details: null,
});

/**
 * The Cluster time one sync may spend, its reads and SetVolumeCopy requests together. The CLI gives `volume sync` 60
 * seconds: the placement check's own 5 and these 45 leave 10 for the SQL around them, which no bound cuts.
 */
export const SYNC_CLUSTER_BUDGET_MS = 45_000;

/**
 * Refresh now and a user's sync: the one mirror sync, as the 15-minute cron runs it. It answers once the mirror has the
 * pull, without waiting for it to land; a sync that could do nothing, or that a mirror refused, answers why.
 */
export const refreshMirror = Effect.fn("VolumeCopies.refreshMirror")(function* (
  caller: { readonly userId: string | null; readonly organizationId: string },
  environment: EnvironmentRef,
  volumeId: string,
  read: BlockerReader = readPlacementBlockers,
  inspect: typeof inspectVolumeCopies = inspectVolumeCopies,
  observe: MemberObserver = observeVolumeMembers,
  ask: CopyAsk = askCopy,
) {
  const named = (fallback: StoreRefusal) => Effect.map(
    inspect(caller.organizationId, environment, volumeId).pipe(Effect.orElseSucceed(() => [])),
    (members) => {
      const unheard = unheardOf(members);
      const refusal = unheard.lacks.length > 0 ? lacking(unheard.lacks)
        : unheard.silent.length > 0 ? silent(unheard.silent)
        : unheard.waiting ? waiting()
        : fallback;
      return { ok: false, refusal } as const;
    },
  );
  const refusal = yield* placementRefusal(caller.organizationId, read);
  if (refusal === UNANSWERED) return yield* named(UNANSWERED);
  if (refusal !== null) return { ok: false, refusal } as const;
  const volume = yield* resolveVolume(caller.organizationId, environment, volumeId);
  const answer = yield* syncMirrors(caller.organizationId, { volumeId, name: volume.volumeName, dockerVolume: volume.dockerVolume, environment }, observe, ask)
    .pipe(Effect.result);
  if (Result.isFailure(answer)) return yield* named(UNANSWERED);
  const { reported, sync, lines } = answer.success;
  if (sync.kind === "failed" && reported.length === 0) return yield* named(UNANSWERED);
  const refused = sync.kind === "synced" && sync.mirrors.some((step) => step.kind === "refused");
  const unfinished = (kind: string) => kind === "unknown" || kind === "unasked" || kind === "failed";
  const unknown = unfinished(sync.kind) || (sync.kind === "synced" && sync.mirrors.some((step) => unfinished(step.kind)));
  if (!refused && !unknown && (sync.kind === "synced" || sync.kind === "demoting")) return { ok: true, lines } as const;
  if (sync.kind === "unanswered") return { ok: false, refusal: silent(sync.servers, failureLines(reported)) } as const;
  if (sync.kind === "lacks") return { ok: false, refusal: lacking(sync.lacks, failureLines(reported)) } as const;
  if (sync.kind === "waiting") return { ok: false, refusal: waiting(failureLines(reported)) } as const;
  if (!refused && unknown) return { ok: false, refusal: { code: "unanswered", message: lines.join("; "), details: null } satisfies StoreRefusal } as const;
  return { ok: false, refusal: { code: "conflict", message: lines.join("; "), details: null } satisfies StoreRefusal } as const;
}, withClusterBudget(SYNC_CLUSTER_BUDGET_MS));

/** What the mirror syncs saw of each of the Volume's mirrors. */
export const mirrorStatuses = Effect.fn("VolumeCopies.mirrorStatuses")(function* (organizationId: string, volumeId: string) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.select().from(volumeMirrorStatus)
    .where(and(eq(volumeMirrorStatus.organizationId, organizationId), eq(volumeMirrorStatus.volumeId, volumeId)));
  return rows.map((row): MirrorStatusView => ({
    machine: row.machineId,
    okAt: row.okAt?.toISOString() ?? null,
    matchedAt: row.matchedAt?.toISOString() ?? null,
    failedAt: row.failedAt?.toISOString() ?? null,
    failure: row.failure,
  }));
});
