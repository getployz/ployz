import { PgDialect } from "drizzle-orm/pg-core";
import { describe, expect, it } from "vitest";
import { unpublishedDestructiveVolumeAttemptFilter } from "#/modules/operations/destructive-volume-attempt-queries.server";

describe("destructive volume outbox query", () => {
  it("recovers fresh attempts and accepted observation retries only", () => {
    const filter = unpublishedDestructiveVolumeAttemptFilter();
    if (!filter) throw new Error("expected dispatch eligibility filter");
    const query = new PgDialect().sqlToQuery(filter);

    expect(query.sql).toContain('"destructive_volume_attempt"."disposition" = $');
    expect(query.sql).toContain(
      '"destructive_volume_retry_source"."disposition" in ($',
    );
    expect(query.sql).toContain(
      '"destructive_volume_retry_source"."operation_id" = "destructive_volume_attempt"."operation_id"',
    );
    expect(query.sql).toContain(
      '"destructive_volume_retry_source"."start_sequence" = "destructive_volume_attempt"."start_sequence"',
    );
    expect(query.params).toEqual([
      "applied",
      "active",
      "accepted",
      "cloud_timeout",
      "cloud_cancelled",
    ]);
  });
});
