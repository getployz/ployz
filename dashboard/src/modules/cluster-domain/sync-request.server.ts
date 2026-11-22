import "@tanstack/react-start/server-only";
import { Effect, Logger } from "effect";
import { clusterDomainSyncRequested } from "#/modules/cluster-domain/cluster-domain.events";
import { onIngressChange } from "#/modules/cluster-domain/ingress-running";
import { InngestClient, sendInngestEvent } from "#/modules/inngest/client";
import type { RuntimeWatchTap } from "#/modules/runtime/runtime-watch.server";
import { redactedLoggers } from "#/server/log-redaction";

/** Ask for a Cluster Domain sync and let the caller see a failed send. */
export const sendClusterDomainSyncRequest = (organizationId: string) =>
  sendInngestEvent(clusterDomainSyncRequested.create({ organizationId }));

/** Ask for a Cluster Domain sync. A lost request only waits for the hourly sync, so a failed send is logged and dropped. */
export const requestClusterDomainSync = (organizationId: string) =>
  sendClusterDomainSyncRequest(organizationId).pipe(
    Effect.catch((error) => Effect.logWarning("Cluster Domain sync request failed; the hourly sync covers it.", error)),
  );

/** The runtime watch's hook: passes each Organization's frames on and requests a sync whenever the Ingress Proxy moves. */
export const clusterDomainSyncTap = Effect.fn("ClusterDomain.syncTap")(function* () {
  const inngest = yield* InngestClient;
  const tap: RuntimeWatchTap = (organizationId) => (frames) =>
    onIngressChange(frames, () =>
      Effect.runFork(
        requestClusterDomainSync(organizationId).pipe(
          Effect.provideService(InngestClient, inngest),
          Effect.provideService(Logger.CurrentLoggers, redactedLoggers),
        ),
      ),
    );
  return tap;
});
