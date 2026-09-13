import { getTableConfig, PgDialect } from "drizzle-orm/pg-core";
import { describe, expect, it } from "vitest";
import {
  machineEnrollmentToken,
  enrollmentAllocation,
  organizationMachine,
  organizationPairing,
} from "#/db/schema";
import { asString } from "#/lib/json";

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

describe("organization machine enrollment invariants", () => {
  it("scopes allocation history to organization and Cluster generation", () => {
    const table = getTableConfig(enrollmentAllocation);
    expect(table.primaryKeys[0]?.columns.map(columnName)).toEqual(["organization_id", "cluster_key"]);
    expect(table.checks.map((check) => check.name)).toEqual([
      "enrollment_allocation_cluster_key_check", "enrollment_allocation_assignments_check",
    ]);
  });

  it("keeps token persistence authorization-only", () => {
    const columns = getTableConfig(machineEnrollmentToken).columns.map(
      columnName,
    );
    expect(columns).toEqual([
      "id",
      "organization_id",
      "token_hash",
      "created_by_user_id",
      "expires_at",
      "created_at",
      "updated_at",
    ]);
  });

  it("stores pending and ready founding state on Organization Pairing", () => {
    const table = getTableConfig(organizationPairing);
    const founderKey = table.columns.find(
      (column) => columnName(column) === "founder_public_key",
    );
    const stateCheck = table.checks.find(
      (candidate) => candidate.name === "organization_pairing_state_check",
    );
    const machineCheck = table.checks.find(
      (candidate) =>
        candidate.name === "organization_pairing_founder_machine_id_check",
    );

    expect(founderKey?.notNull).toBe(false);
    expect(stateCheck).toBeDefined();
    if (!stateCheck) return;
    const stateQuery = new PgDialect().sqlToQuery(stateCheck.value);
    expect(stateQuery.sql).toContain("founder_public_key");
    expect(stateQuery.sql).toContain("founder_machine_id");
    expect(machineCheck).toBeDefined();
    if (!machineCheck) return;
    const query = new PgDialect().sqlToQuery(machineCheck.value);
    expect(query.sql).toContain("founder_machine_id");
    expect(query.sql).toContain("^[0-9a-f]{32}$");
  });

  it("uniques rust Machine ids only with Organization ids", () => {
    const table = getTableConfig(organizationMachine);
    const primary = table.primaryKeys[0];
    const machineIndex = table.indexes.find(
      (candidate) =>
        candidate.config.name === "organization_machine_machine_idx",
    );

    expect(primary?.columns.map(columnName)).toEqual([
      "organization_id",
      "machine_id",
    ]);
    expect(machineIndex?.config.unique).toBe(false);
    expect(machineIndex?.config.columns.map(columnName)).toEqual(["machine_id"]);
  });

});
