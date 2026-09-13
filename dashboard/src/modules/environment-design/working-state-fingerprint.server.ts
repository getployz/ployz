import "@tanstack/react-start/server-only";

import { createHash } from "node:crypto";
import {
  canonicalReviewedEnvironmentWorkingStateJson,
  formatReviewedEnvironmentWorkingStateFingerprint,
  type ReviewedEnvironmentWorkingState,
} from "#/modules/environment-design/working-state-review";

export function fingerprintReviewedEnvironmentWorkingStateSync(
  input: ReviewedEnvironmentWorkingState,
): string {
  return formatReviewedEnvironmentWorkingStateFingerprint(
    createHash("sha256")
      .update(canonicalReviewedEnvironmentWorkingStateJson(input))
      .digest("hex"),
  );
}
