import { getTableConfig, PgDialect } from "drizzle-orm/pg-core";
import { describe, expect, it } from "vitest";
import { asString } from "#/lib/json";
import { teardownAttempt } from "#/db/schema";

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

describe("teardown attempt database invariants", () => {
  it("allows only one active attempt per environment, project, or organization", () => {
    const config = getTableConfig(teardownAttempt);
    const environment = config.indexes.find(
      ({ config: index }) =>
        index.name === "teardown_attempt_one_active_environment_idx",
    );
    const project = config.indexes.find(
      ({ config: index }) =>
        index.name === "teardown_attempt_one_active_project_idx",
    );
    const organization = config.indexes.find(
      ({ config: index }) =>
        index.name === "teardown_attempt_one_active_organization_idx",
    );

    expect(environment?.config.unique).toBe(true);
    expect(environment?.config.columns.map(columnName)).toEqual([
      "environment_id",
    ]);
    expect(environment?.config.where).toBeDefined();
    expect(project?.config.unique).toBe(true);
    expect(organization?.config.unique).toBe(true);
  });

  it("stores a unique Inngest run id", () => {
    const config = getTableConfig(teardownAttempt);
    const runIndex = config.indexes.find(
      ({ config: index }) => index.name === "teardown_attempt_inngest_run_uidx",
    );
    expect(runIndex?.config.unique).toBe(true);
  });

  it("does not foreign-key organization, project, or environment so leftover rust stays retryable after Cloud rows drop", () => {
    const foreignKeys = getTableConfig(teardownAttempt).foreignKeys;
    const columnNames = foreignKeys.flatMap(({ reference }) =>
      reference().columns.map(({ name }) => name),
    );

    expect(columnNames).not.toContain("organization_id");
    expect(columnNames).not.toContain("project_id");
    expect(columnNames).not.toContain("environment_id");
  });

  it("requires run ownership for running and terminal statuses", () => {
    const statusCheck = getTableConfig(teardownAttempt).checks.find(
      ({ name }) => name === "teardown_attempt_status_shape_check",
    );
    expect(statusCheck).toBeDefined();
    if (!statusCheck) return;
    const rendered = new PgDialect().sqlToQuery(statusCheck.value).sql;
    expect(rendered).toContain("'pending'");
    expect(rendered).toContain("'running'");
    expect(rendered).toContain("'completed'");
    expect(rendered).toContain("'partial'");
    expect(rendered).toContain("'failed'");
    expect(rendered).toContain("'cancelled'");
  });
});
