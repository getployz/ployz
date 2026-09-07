import { describe, expect, it } from "vitest";
import { EffectDrizzleQueryError } from "drizzle-orm/effect-core";
import { Cause } from "effect";
import { SqlError, UniqueViolation } from "effect/unstable/sql/SqlError";
import { isMachineRemoveUniqueViolation } from "#/modules/machines/machine-removal.repository";

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
    query: "insert into machine_remove_attempt",
    params: [],
    cause: Cause.fail(new SqlError({ reason: uniqueViolation(constraint) })),
  });
}

describe("machine remove unique violation", () => {
  it("treats a drizzle-wrapped UniqueViolation on the active org-machine index as contention", () => {
    expect(
      isMachineRemoveUniqueViolation(
        drizzleWrappedSqlError(
          "machine_remove_attempt_one_active_org_machine_idx",
        ),
      ),
    ).toBe(true);
  });

  it("treats a typed UniqueViolation on the inngest run index as contention", () => {
    expect(
      isMachineRemoveUniqueViolation(
        uniqueViolation("machine_remove_attempt_inngest_run_uidx"),
      ),
    ).toBe(true);
  });

  it("does not treat UniqueViolation on an unrelated constraint as contention", () => {
    expect(
      isMachineRemoveUniqueViolation(drizzleWrappedSqlError("users_email_idx")),
    ).toBe(false);
  });
});
