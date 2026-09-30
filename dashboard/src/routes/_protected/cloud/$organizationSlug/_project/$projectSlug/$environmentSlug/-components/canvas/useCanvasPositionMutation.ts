import { useServerFn } from "@tanstack/react-start";
import { createOptimisticAction, usePacedMutations, throttleStrategy } from "@tanstack/react-db";
import { toast } from "sonner";
import { toErrorMessage } from "#/lib/error-message";
import { type OnNodeDrag } from "@xyflow/react";
import { getCanvasPositionsCollection } from "#/collections/collections";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { canvasPositionKey, type CanvasPosition, type UpdateCanvasPositionInput } from "#/modules/canvas/canvas-positions";
import { updateCanvasPositionServerFn } from "#/modules/canvas/canvas-positions.functions";
import type { CanvasResourceNode, CanvasResourceType } from "./types";

export function useCanvasPositionMutation(params: {
  organizationId: string;
  organizationSlug: string;
  projectSlug: string;
  environmentSlug: string;
}) {
  const collection = getCanvasPositionsCollection(params.organizationSlug, useCollectionScope());
  const updatePosition = useServerFn(updateCanvasPositionServerFn);

  function writeLocalPosition(input: {
    environmentId: string;
    resourceType: CanvasResourceType;
    resourceId: string;
    x: number;
    y: number;
  }) {
    const nextX = Math.round(input.x);
    const nextY = Math.round(input.y);
    const collectionKey = canvasPositionKey(input);
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
          const modified = m.modified as CanvasPosition;
          return updatePosition({ data: {
            organizationSlug: params.organizationSlug,
            environmentId: modified.environmentId,
            // SAFETY: the canvas writes only the types it draws.
            resourceType: modified.resourceType as CanvasResourceType,
            resourceId: modified.resourceId,
            x: Math.round(modified.x),
            y: Math.round(modified.y),
          } });
        }),
        collection,
      );
    },
    strategy: throttleStrategy({ wait: 250, leading: false, trailing: true }),
  });

  const onNodeDrag: OnNodeDrag<CanvasResourceNode> = (_event, node) => {
    // Live Nodes sit where their owner put them.
    if (node.type === "storeLive") return;
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
  writes: readonly Promise<Awaited<ReturnType<typeof updateCanvasPositionServerFn>>>[],
  collection: ReturnType<typeof getCanvasPositionsCollection>,
) {
  const results = await Promise.allSettled(writes);
  const committed = results.flatMap((result) => result.status === "fulfilled" ? [result.value.data] : []);
  if (committed.length) await collection.writeCommitted(committed);
  const failure = results.find((result) => result.status === "rejected");
  if (failure) throw failure.reason;
}

/**
 * Places a node the user is creating where they put it: shown at once, saved in the background, and back to a free
 * spot (with a toast) if the save fails.
 */
export function usePlaceNewNode(organizationSlug: string) {
  const collection = getCanvasPositionsCollection(organizationSlug, useCollectionScope());
  const updatePosition = useServerFn(updateCanvasPositionServerFn);
  const place = createOptimisticAction<Omit<UpdateCanvasPositionInput, "organizationSlug">>({
    onMutate: (input) => {
      const now = new Date();
      // ponytail: the organization id is Cloud's to fill; the canvas never reads it (its schema leaves it out).
      collection.insert({ ...input, id: crypto.randomUUID(), organizationId: "", x: Math.round(input.x), y: Math.round(input.y),
        createdAt: now, updatedAt: now });
    },
    mutationFn: (input) => persistCanvasPositionBatch([updatePosition({ data: { ...input, organizationSlug,
      x: Math.round(input.x), y: Math.round(input.y) } })], collection),
  });
  return (input: Omit<UpdateCanvasPositionInput, "organizationSlug">) => {
    place(input).isPersisted.promise.catch((error: Error) =>
      toast.error(toErrorMessage(error, "The new node's place couldn't be saved.")));
  };
}
