import { useServerFn } from "@tanstack/react-start";
import { usePacedMutations, throttleStrategy } from "@tanstack/react-db";
import { type OnNodeDrag } from "@xyflow/react";
import { updateEnvironmentResourceCanvasPositionServerFn } from "#/modules/environment-design/resource-functions";
import { updateServiceCanvasPositionServerFn } from "#/modules/environment-design/service-functions";
import {
  getCanvasPositionCollectionKey,
  useCanvasPositionsCollection,
} from "#/modules/services/services.collection";
import type { ServiceCanvasPositionRecord } from "#/modules/environment-design/services";
import type { CanvasResourceNode, CanvasResourceType } from "./types";

export function useCanvasPositionMutation(params: {
  organizationId: string;
  organizationSlug: string;
  projectSlug: string;
  environmentSlug: string;
}) {
  const collection = useCanvasPositionsCollection(params.organizationSlug);
  const updateServicePosition = useServerFn(updateServiceCanvasPositionServerFn);
  const updateResourcePosition = useServerFn(
    updateEnvironmentResourceCanvasPositionServerFn,
  );

  function writeLocalPosition(input: {
    environmentId: string;
    resourceType: CanvasResourceType;
    resourceId: string;
    x: number;
    y: number;
  }) {
    const nextX = Math.round(input.x);
    const nextY = Math.round(input.y);
    const collectionKey = getCanvasPositionCollectionKey(input);
    const existing = collection.get(collectionKey);
    const now = new Date();

    if (existing) {
      collection.update(collectionKey, (draft) => {
        draft["environmentId"] = input.environmentId;
        draft["resourceType"] = input.resourceType;
        draft["resourceId"] = input.resourceId;
        draft["x"] = nextX;
        draft["y"] = nextY;
        draft["updatedAt"] = now;
      });
      return;
    }

    collection.insert({
      id: crypto.randomUUID(),
      organizationId: params.organizationId,
      environmentId: input.environmentId,
      resourceType: input.resourceType,
      resourceId: input.resourceId,
      x: nextX,
      y: nextY,
      createdAt: now,
      updatedAt: now,
    });
  }

  const mutate = usePacedMutations({
    onMutate: ({
      environmentId,
      resourceType,
      resourceId,
      x,
      y,
    }: {
      environmentId: string;
      resourceType: CanvasResourceType;
      resourceId: string;
      x: number;
      y: number;
    }) => {
      writeLocalPosition({
        environmentId,
        resourceType,
        resourceId,
        x,
        y,
      });
    },
    mutationFn: async ({ transaction }) => {
      await persistCanvasPositionBatch(
        transaction.mutations.map(async (m) => {
          // SAFETY: this paced mutation only writes canvas position rows; TanStack DB types `modified` as a generic mutation payload.
          const modified = m.modified as ServiceCanvasPositionRecord;
          const data = {
            organizationSlug: params.organizationSlug,
            environmentId: modified.environmentId,
            x: Math.round(modified.x),
            y: Math.round(modified.y),
          };

          if (modified.resourceType === "service") {
            return updateServicePosition({
              data: { ...data, serviceId: modified.resourceId },
            });
          }

          return updateResourcePosition({
            data: {
              ...data,
              resourceId: modified.resourceId,
            },
          });
        }),
        collection,
      );
    },
    strategy: throttleStrategy({ wait: 250, leading: false, trailing: true }),
  });

  const onNodeDrag: OnNodeDrag<CanvasResourceNode> = (_event, node) => {
    mutate({
      environmentId: node.data.environmentId,
      resourceType: node.data.resourceType,
      resourceId: node.data.resourceId,
      x: node.position.x,
      y: node.position.y,
    });
  };

  return {
    onNodeDrag,
  };
}

export async function persistCanvasPositionBatch(
  writes: readonly Promise<Awaited<ReturnType<typeof updateServiceCanvasPositionServerFn>>>[],
  collection: ReturnType<typeof useCanvasPositionsCollection>,
) {
  const results = await Promise.allSettled(writes);
  const committed = results.flatMap((result) => result.status === "fulfilled" ? [result.value.data] : []);
  if (committed.length) await collection.writeCommitted(committed);
  const failure = results.find((result) => result.status === "rejected");
  if (failure) throw failure.reason;
}
