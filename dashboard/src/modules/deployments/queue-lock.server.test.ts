import { describe, expect, it } from "vitest";
import { EffectDrizzleQueryError } from "drizzle-orm/effect-core";
import { Cause } from "effect";
import {
  SqlError,
  UniqueViolation,
} from "effect/unstable/sql/SqlError";
import { isActiveDeploymentUniqueViolation } from "#/modules/deployments/queue-lock.server";

function uniqueViolation(constraint: string) {
  return new UniqueViolation({
    cause: Object.assign(new Error("duplicate key value violates unique constraint"), {
      code: "23505",
      constraint,
    }),
    constraint,
  });
}

function drizzleWrappedSqlError(constraint: string) {
  return new EffectDrizzleQueryError({
    query: "update environment_deployment set status = $1",
    params: ["planning"],
    cause: Cause.fail(new SqlError({ reason: uniqueViolation(constraint) })),
  });
}

describe("active deployment unique violation", () => {
  it("treats a drizzle-wrapped UniqueViolation on the started-attempt index as queue contention", () => {
    expect(
      isActiveDeploymentUniqueViolation(
        drizzleWrappedSqlError(
          "environment_deployment_one_started_attempt_idx",
        ),
      ),
    ).toBe(true);
  });

  it("treats a typed UniqueViolation on the queued-target index as queue contention", () => {
    expect(
      isActiveDeploymentUniqueViolation(
        uniqueViolation("environment_deployment_one_queued_target_idx"),
      ),
    ).toBe(true);
  });

  it("does not treat UniqueViolation on an unrelated constraint as queue contention", () => {
    expect(
      isActiveDeploymentUniqueViolation(
        drizzleWrappedSqlError("users_email_idx"),
      ),
    ).toBe(false);
  });
});
