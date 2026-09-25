import { Effect, Option, Schema } from "effect";
import { loadClusterDomain } from "#/modules/cluster-domain/cluster-domain.server";
import {
  ensureClusterDomainCertificate,
  listClusterDomainOrganizationIds,
  probeIngressServers,
  publishClusterDomainCertificate,
  publishClusterDomainRecords,
  recordClusterDomainCheck,
  renewClusterDomainLease,
} from "#/modules/cluster-domain/sync.server";
import type { PloyzInngest, PloyzStepTools } from "#/modules/inngest/client";
import {
  clusterDomainSyncRequestedEventType,
  createClusterDomainSyncRequestedEvent,
} from "#/modules/inngest/events";
import { runInngestEffect } from "#/server/run.server";

type StepTools = Pick<PloyzStepTools, "run" | "sendEvent">;
type EffectRunner = typeof runInngestEffect;

const SyncRequestedData = Schema.Struct({ organizationId: Schema.String.check(Schema.isNonEmpty()) });

/**
 * Keeps the Organization's Cluster Domain correct: skip when none is reserved → probe ingress Servers →
 * record what the probe found → full-set records PUT when something answered (which renews the lease)
 * → otherwise renew the lease → replace the wildcard certificate when missing or near expiry →
 * republish it to the Cluster. With no Cluster only the lease and certificate steps do anything.
 */
export async function executeSyncClusterDomain(
  { event, step }: { event: { data: unknown }; step: StepTools },
  runEffect: EffectRunner,
) {
  const organizationId = await step.run("normalize-organization-id", () => {
    const decoded = Schema.decodeUnknownOption(SyncRequestedData)(event.data, { onExcessProperty: "preserve" });
    return Option.isSome(decoded) ? decoded.value.organizationId : null;
  });
  if (organizationId === null) return { organizationId: null, skipped: true };

  // Only a deployment that needs a generated hostname reserves the name.
  const name = await step.run("load-name", () =>
    runEffect(loadClusterDomain(organizationId).pipe(Effect.map((row) => row?.name ?? null))));
  if (name === null) return { organizationId, skipped: true };
  const probe = await step.run("probe-ingress-servers", () => runEffect(probeIngressServers(organizationId)));
  await step.run("record-check", () => runEffect(recordClusterDomainCheck(organizationId, probe)));
  const reachable = probe.kind === "probed" ? probe.reachable : [];
  // Hosted DNS refuses an empty set, and the last good set is better than none.
  const recordsPut = reachable.length > 0;
  if (recordsPut) await step.run("publish-records", () => runEffect(publishClusterDomainRecords(organizationId, reachable)));
  else await step.run("renew-lease", () => runEffect(renewClusterDomainLease(organizationId)));
  // Issuance can take minutes; the connect worker has no serve-style HTTP timeout, so the step waits it out.
  const certificateIssued = await step.run("ensure-certificate", () => runEffect(ensureClusterDomainCertificate(organizationId)));
  const certificatePublished = await step.run("publish-certificate", () => runEffect(publishClusterDomainCertificate(organizationId)));
  return { organizationId, name, observed: probe.kind !== "unknown", recordsPut, certificateIssued, certificatePublished };
}

export async function executeScheduleClusterDomainSync({ step }: { step: StepTools }, runEffect: EffectRunner) {
  const organizationIds = await step.run("list-cluster-domain-organization-ids", () => runEffect(listClusterDomainOrganizationIds()));
  if (organizationIds.length > 0) {
    await step.sendEvent(
      "request-cluster-domain-syncs",
      organizationIds.map((organizationId) => createClusterDomainSyncRequestedEvent({ organizationId })),
    );
  }
  return { organizationCount: organizationIds.length };
}

export const createSyncClusterDomain = (inngest: PloyzInngest, runEffect: EffectRunner = runInngestEffect) =>
  inngest.createFunction(
    {
      id: "sync-cluster-domain",
      retries: 3,
      triggers: [{ event: clusterDomainSyncRequestedEventType }],
      concurrency: [{ key: "event.data.organizationId", limit: 1 }],
    },
    async ({ event, step }) => executeSyncClusterDomain({ event, step }, runEffect),
  );

export const createScheduleClusterDomainSync = (inngest: PloyzInngest, runEffect: EffectRunner = runInngestEffect) =>
  inngest.createFunction(
    {
      id: "schedule-cluster-domain-sync",
      retries: 3,
      triggers: [{ cron: "TZ=UTC 0 * * * *" }],
      concurrency: [{ limit: 1 }],
    },
    async ({ step }) => executeScheduleClusterDomainSync({ step }, runEffect),
  );
