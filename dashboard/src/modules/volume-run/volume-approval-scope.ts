import type { EnvironmentRef, MachineId, ResolvedServiceSpec } from "@ployz/sdk";
import type { QueryClient } from "@tanstack/react-query";
import { type VolumeScope, VolumeNotApproved } from "#/modules/approval-session/volume-approval";
import { sealingQueryOptions } from "#/modules/sealing/sealing.queries";
import { volumeApprovalTermsServerFn } from "#/modules/volume-run/volume-run.functions";

/**
 * A Volume's scope read afresh for an approval, with its recorded size, `project/environment` it lives in and, for a Move
 * off `from`, the spec it runs there.
 */
export type VolumeTermsRead = VolumeScope & { readonly recorded: number | null; readonly place: string; readonly running: ResolvedServiceSpec | null };

/**
 * `volumeId`'s names, its Servers' answers and its Cluster's public key, each read afresh, since an approval names them
 * all. `from` also reads the spec of the Service a Move off that Server would start on its target.
 */
export async function readVolumeScope(
  queryClient: QueryClient,
  organizationSlug: string,
  environment: EnvironmentRef,
  volumeId: string,
  from: MachineId | null = null,
): Promise<VolumeTermsRead> {
  const [sealing, terms] = await Promise.all([
    queryClient.fetchQuery({ ...sealingQueryOptions(organizationSlug), staleTime: 0 }),
    volumeApprovalTermsServerFn({ data: { organizationSlug, volumeId, environment, from } }),
  ]);
  if (sealing.sealing.state !== "ready") throw new VolumeNotApproved("No server has joined yet, so there is nothing to approve.");
  return { ...terms, cluster: sealing.sealing.publicKey };
}
