import type { DestructiveVolumeReview } from "#/modules/deployments/deployment-contract";
import type { PreparedVolumeDestruction } from "./volume-destruction-confirmation-dialog";

export function prepareVolumeDestructionReview(input: {
  reviews: readonly DestructiveVolumeReview[];
  expectedResourceIds: readonly string[];
  expectedNamespaceId: string;
}): PreparedVolumeDestruction {
  const reviews = [...input.reviews].sort((left, right) =>
    left.target.resourceId.localeCompare(right.target.resourceId),
  );
  const actualIds = reviews.map((review) => review.target.resourceId);
  const expectedIds = [...new Set(input.expectedResourceIds)].sort();
  if (
    actualIds.length !== expectedIds.length ||
    actualIds.some((resourceId, index) => resourceId !== expectedIds[index])
  ) {
    throw new Error(
      "The deployed volumes awaiting removal changed. Refresh the canvas before reviewing this deployment.",
    );
  }

  for (const review of reviews) {
    if (
      review.target.namespaceId !== input.expectedNamespaceId ||
      review.evidence.evidence.namespaceId !== input.expectedNamespaceId
    ) {
      throw new Error(
        "Fresh volume evidence belongs to a different environment namespace.",
      );
    }
    if (
      review.target.volumeName !== review.evidence.evidence.volumeName ||
      review.target.machineId !== review.evidence.evidence.machineId
    ) {
      throw new Error(
        "Fresh volume evidence does not match its removal target.",
      );
    }
  }

  const referencingServices = [
    ...new Set(
      reviews.flatMap((review) => review.evidence.evidence.referencingServices),
    ),
  ].sort();

  return {
    namespaceId: input.expectedNamespaceId,
    reviews,
    volumes: reviews.map((review) => ({
      evidence: review.evidence.evidence,
      fingerprint: review.evidence.fingerprint,
    })),
    referencingServices,
    fingerprint: JSON.stringify(reviews),
  };
}
