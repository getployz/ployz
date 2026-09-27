import "@tanstack/react-start/server-only";

import { and, eq } from "drizzle-orm";
import { Effect } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { withoutSealedCiphertext } from "#/modules/environment-design/saved-intent";
import { environmentBranch as schemaEnvironmentBranch } from "#/modules/project/tables";
import { requireInfrastructureOrganization } from "#/modules/runtime/organization-access.server";
import type { BranchCloseReason } from "#/modules/runtime/teardown";
import { confirmSystemTeardown } from "#/modules/runtime/teardown.server";
import { Database } from "#/server/database.server";
import { NotFound } from "#/server/public-error";

/**
 * The system closes a Branch (after a Merge, or when it sits idle) through the Environment teardown, which takes its
 * own Branches first. It runs as the Branch's creator; a person closes one through the teardown's typed confirmation.
 */
export const closeBranch = Effect.fn("Branches.close")(function* (input: {
  readonly environmentId: string;
  readonly reason: Exclude<BranchCloseReason, "manual">;
}) {
  const database = yield* Database;
  const [branch] = yield* database.drizzle
    .select({ createdByUserId: schemaEnvironmentBranch.createdByUserId })
    .from(schemaEnvironmentBranch)
    .where(eq(schemaEnvironmentBranch.environmentId, input.environmentId));
  if (branch === undefined) return yield* new NotFound({ message: "The branch was not found." });
  return yield* confirmSystemTeardown({
    environmentId: input.environmentId,
    requestedByUserId: branch.createdByUserId,
    closeReason: input.reason,
  });
});

/** A kept Branch stays after merging and never closes for being idle. */
export const setBranchKept = Effect.fn("Branches.setKept")(function* (actor: Actor, input: {
  readonly organizationSlug: string;
  readonly environmentId: string;
  readonly kept: boolean;
}) {
  const organization = yield* requireInfrastructureOrganization(actor, input.organizationSlug);
  const database = yield* Database;
  const [row] = yield* database.drizzle
    .update(schemaEnvironmentBranch)
    .set({ kept: input.kept })
    .where(and(
      eq(schemaEnvironmentBranch.environmentId, input.environmentId),
      eq(schemaEnvironmentBranch.organizationId, organization.id),
    ))
    .returning();
  if (row === undefined) return yield* new NotFound({ message: "The branch was not found." });
  return { ...row, base: withoutSealedCiphertext(row.base) };
});
