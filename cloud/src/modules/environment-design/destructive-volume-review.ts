import { Data, Schema } from "effect";
import { areDeepEqual } from "#/utils/schema-path";
import { finiteNumber } from "./schema";

const NonEmptyString = Schema.String.check(Schema.isNonEmpty());
const NonNegativeInteger = finiteNumber({ integer: true, minimum: 0 });
const isoDateTime =
  /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})$/u;
const DateTimeString = Schema.String.check(
  Schema.makeFilter<string>((value) =>
    isoDateTime.test(value) && !Number.isNaN(Date.parse(value))
      ? undefined
      : "Expected an ISO date-time string.",
  ),
);

const destructiveVolumeEvidenceSchema = Schema.Struct({
  namespaceId: NonEmptyString,
  volumeName: NonEmptyString,
  machineId: NonEmptyString,
  kind: Schema.Union([
    Schema.Struct({ kind: Schema.Literal("plain") }),
    Schema.Struct({
      kind: Schema.Literal("provisioned"),
      dataset: NonEmptyString,
      maxSizeBytes: NonNegativeInteger,
    }),
  ]),
  availability: Schema.Union([
    Schema.Struct({
      status: Schema.Literal("available"),
      usedBytes: NonNegativeInteger,
      lastWriteUnixSeconds: NonNegativeInteger,
    }),
    Schema.Struct({ status: Schema.Literal("unavailable") }),
    Schema.Struct({ status: Schema.Literal("no_answer") }),
  ]),
  referencingServices: Schema.mutable(Schema.Array(NonEmptyString)),
});

export const reviewedDestructiveVolumeTargetSchema = Schema.Struct({
  version: Schema.Literal(1),
  resourceId: NonEmptyString,
  namespaceId: NonEmptyString,
  volumeName: NonEmptyString,
  machineId: NonEmptyString,
});

export const reviewedDestructiveVolumeEvidenceSchema = Schema.Struct({
  version: Schema.Literal(1),
  fingerprint: NonEmptyString,
  reviewedAt: DateTimeString,
  evidence: destructiveVolumeEvidenceSchema,
});

export const destructiveVolumeReviewSchema = Schema.Struct({
  target: reviewedDestructiveVolumeTargetSchema,
  evidence: reviewedDestructiveVolumeEvidenceSchema,
});

export const destructiveVolumeReviewsSchema = Schema.mutable(
  Schema.Array(destructiveVolumeReviewSchema),
).check(
  Schema.makeFilter((reviews) => {
    const resourceIds = new Set<string>();
    const issues: Schema.FilterIssue[] = [];
    for (const [index, review] of reviews.entries()) {
      if (resourceIds.has(review.target.resourceId)) {
        issues.push({
          path: [index, "target", "resourceId"],
          issue: "A destructive volume can be reviewed only once.",
        });
      }
      resourceIds.add(review.target.resourceId);
    }
    return issues;
  }),
);

export type DestructiveVolumeReview = typeof destructiveVolumeReviewSchema.Type;

/**
 * Carry deletion authority across unrelated Saved revisions, but revoke it as
 * soon as the volume is authored back into the desired graph. Fresh review
 * evidence wins for the same resource.
 */
export function resolveSavedVolumeDeletionAuthorizations(input: {
  previous: readonly DestructiveVolumeReview[];
  fresh: readonly DestructiveVolumeReview[];
  presentVolumeIds: ReadonlySet<string>;
}) {
  const byResourceId = new Map(
    [...input.previous, ...input.fresh].map((review) => [
      review.target.resourceId,
      review,
    ]),
  );
  return [...byResourceId.values()]
    .filter((review) => !input.presentVolumeIds.has(review.target.resourceId))
    .sort((left, right) =>
      left.target.resourceId.localeCompare(right.target.resourceId),
    );
}

export class DestructiveVolumeReviewChangedError extends Data.TaggedError(
  "DestructiveVolumeReviewChangedError",
)<{
  readonly reason: "review_updated_evidence";
  readonly message: string;
  readonly freshReviews: DestructiveVolumeReview[];
}> {}

export function getDestructiveVolumeReviewMismatch(input: {
  reviewed: readonly DestructiveVolumeReview[];
  fresh: readonly DestructiveVolumeReview[];
}) {
  const reviewedByResourceId = new Map(
    input.reviewed.map((review) => [review.target.resourceId, review]),
  );
  const freshByResourceId = new Map(
    input.fresh.map((review) => [review.target.resourceId, review]),
  );
  if (
    reviewedByResourceId.size !== input.reviewed.length ||
    freshByResourceId.size !== input.fresh.length
  ) {
    return "Destructive volume review contains duplicate resources.";
  }
  if (reviewedByResourceId.size !== freshByResourceId.size) {
    return "The set of deployed volumes awaiting removal changed after review.";
  }
  for (const [resourceId, fresh] of freshByResourceId) {
    const reviewed = reviewedByResourceId.get(resourceId);
    if (!reviewed) {
      return "A deployed volume awaiting removal was not reviewed.";
    }
    if (
      !areDeepEqual(reviewed.target, fresh.target) ||
      reviewed.evidence.fingerprint !== fresh.evidence.fingerprint ||
      !areDeepEqual(reviewed.evidence.evidence, fresh.evidence.evidence)
    ) {
      return `Volume ${fresh.target.volumeName} changed after review. Review the fresh evidence before deploying.`;
    }
  }
  return null;
}
