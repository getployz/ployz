import { Schema } from "effect";
import { dataLossIdentitySchema } from "#/modules/runtime/data-loss-identity";

/** A request no run picked up in this long never starts; the hourly sweep closes it. */
export const CLEANUP_PENDING_LIMIT_MS = 15 * 60_000;
/** A clean running this long lost its run; the hourly sweep closes it as lost. */
export const CLEANUP_RUNNING_LIMIT_MS = 60 * 60_000;

/** The dashboard's clean of one stray Namespace, deleting exactly the Data Loss its dialog showed. */
export const RemoveNamespaceInput = Schema.Struct({
  organizationSlug: Schema.NonEmptyString,
  namespace: Schema.NonEmptyString,
  confirmDataLoss: Schema.Array(dataLossIdentitySchema),
});
