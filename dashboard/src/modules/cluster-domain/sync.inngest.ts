import { Effect, Option, Schema } from "effect";
import { reserveClusterDomain } from "#/modules/cluster-domain/cluster-domain.server";
import {
  ensureClusterDomainCertificate,
  listClusterDomainOrganizationIds,
  probeIngressServers,
  publishClusterDomainCertificate,
  publishClusterDomainRecords,
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
 * Keeps the Organization's Cluster Domain correct: reserve if missing → probe ingress Servers →
 * full-set records PUT when a frame was read and something answered (which renews the lease) →
 * otherwise renew the lease → replace the wildcard certificate when missing or near expiry →
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

  const name = await step.run("reserve", () =>
    runEffect(reserveClusterDomain(organizationId).pipe(Effect.map((row) => row.name))));
  const probe = await step.run("probe-ingress-servers", () => runEffect(probeIngressServers(organizationId)));
  const recordsPut = probe === null
    ? false
    : await step.run("publish-records", () => runEffect(publishClusterDomainRecords(organizationId, probe)));
  if (!recordsPut) await step.run("renew-lease", () => runEffect(renewClusterDomainLease(organizationId)));
  // Issuance can take minutes; the connect worker has no serve-style HTTP timeout, so the step waits it out.
  const certificateIssued = await step.run("ensure-certificate", () => runEffect(ensureClusterDomainCertificate(organizationId)));
  const certificatePublished = await step.run("publish-certificate", () => runEffect(publishClusterDomainCertificate(organizationId)));
  return { organizationId, name, observed: probe !== null, recordsPut, certificateIssued, certificatePublished };
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
