import { useInfiniteQuery } from "@tanstack/react-query";
import { useMemo } from "react";
import type { JsonObject } from "#/db/tables";
import { parseDeployOperationEvidence } from "#/modules/operations/deploy-operation-evidence";
import { deploymentOperationEvidenceQueryOptions } from "#/modules/deployments/deployment-queries";

export type OperationEvidenceRow = {
  sequence: string;
  eventType: string;
  schemaVersion: number;
  payload: JsonObject;
  createdAt: Date;
};
export type ParsedOperationEvidenceRow = OperationEvidenceRow & {
  parsed: ReturnType<typeof parseDeployOperationEvidence> | null;
};

export function useDeploymentOperationEvidence(input: {
  organizationSlug: string;
  deploymentId: string;
  enabled: boolean;
}) {
  const query = useInfiniteQuery(
    deploymentOperationEvidenceQueryOptions(input),
  );
  const events: ParsedOperationEvidenceRow[] = useMemo(
    () =>
      query.data?.pages.flatMap((page) =>
        (page?.events ?? []).map((event) => ({
          ...event,
          parsed:
            event.schemaVersion === 1
              ? parseDeployOperationEvidence(event)
              : null,
        })),
      ) ?? [],
    [query.data?.pages],
  );

  return { ...query, events };
}
