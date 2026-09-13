import { PgDialect, getTableConfig } from "drizzle-orm/pg-core";
import { describe, expect, it } from "vitest";
import { asString } from "#/lib/json";
import { machineRemoveAttempt } from "#/db/schema";

function hasColumnName<T>(column: T): column is T & { name: string } {
  return (
    column !== null &&
    column !== undefined &&
    typeof column === "object" &&
    "name" in column &&
    typeof column.name === "string"
  );
}

function columnName<T>(column: T) {
  return hasColumnName(column) ? column.name : asString(column);
}

function namedCheck(table: ReturnType<typeof getTableConfig>, name: string) {
  return table.checks.find((candidate) => candidate.name === name);
}

describe("machine remove schema invariants", () => {
  it("requires a unique Inngest run id once the durable row is claimed", () => {
    const table = getTableConfig(machineRemoveAttempt);
    const runId = table.indexes.find(
      ({ config }) => config.name === "machine_remove_attempt_inngest_run_uidx",
    );
    const active = table.indexes.find(
      ({ config }) =>
        config.name === "machine_remove_attempt_one_active_org_machine_idx",
    );

    expect(runId?.config.unique).toBe(true);
    expect(runId?.config.columns.map(columnName)).toEqual(["inngest_run_id"]);
    expect(runId?.config.where).toBeDefined();
    expect(active?.config.unique).toBe(true);
    expect(active?.config.columns.map(columnName)).toEqual([
      "organization_id",
      "machine_id",
    ]);
    expect(active?.config.where).toBeDefined();
  });

  it("requires run id on running and terminal states and missing identities as a terminal state", () => {
    const stateCheck = namedCheck(
      getTableConfig(machineRemoveAttempt),
      "machine_remove_attempt_state_shape_check",
    );
    expect(stateCheck).toBeDefined();
    if (!stateCheck) return;

    const rendered = new PgDialect().sqlToQuery(stateCheck.value).sql;
    expect(rendered).toContain("'pending'");
    expect(rendered).toContain("'running'");
    expect(rendered).toContain("'succeeded'");
    expect(rendered).toContain("'failed'");
    expect(rendered).toContain("'cancelled'");
    expect(rendered).toContain("'missing_identities'");
    expect(rendered).toContain("inngest_run_id");
  });
});
