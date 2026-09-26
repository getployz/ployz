import { clusterDomainStatus } from "#/modules/cluster-domain/cluster-domain";
import { useClusterDomain } from "#/modules/cluster-domain/use-cluster-domain";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import { useRuntimeStatus } from "#/providers/runtime-provider";

/** What every public domain row on a Service reads: the Cluster Domain and the Runtime Watch. */
export function usePublicDomainContext(organizationSlug: string) {
  const clusterDomainRow = useClusterDomain(organizationSlug);
  const { machines } = useRuntimeLens(organizationSlug);
  const { lensStatus, certificates } = useRuntimeStatus();
  return {
    /** The Cluster Domain name, or null while none is reserved. */
    clusterDomain: clusterDomainRow?.name ?? null,
    clusterStatus: clusterDomainRow ? clusterDomainStatus(clusterDomainRow, new Date()) : null,
    /** Public addresses of the Servers that accept ingress; A/AAAA records point here. */
    ingressAddresses: machines.flatMap((machine) =>
      machine.acceptsIngress && machine.publicIp !== null ? [machine.publicIp] : []),
    observed: lensStatus === "observed",
    certificates,
  };
}
