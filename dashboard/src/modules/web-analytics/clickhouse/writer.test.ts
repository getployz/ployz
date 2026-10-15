import { expect, it } from "vitest";
import { DEDUPLICATION_WINDOW } from "#/modules/web-analytics/clickhouse/schema";
import { DRAIN_BATCHES, DRAINS_PER_RUN } from "#/modules/web-analytics/clickhouse/writer";

it("every block a drain run can replay is still inside the pageview table's deduplication window", () => {
  expect(DRAIN_BATCHES * DRAINS_PER_RUN).toBeLessThanOrEqual(DEDUPLICATION_WINDOW);
});
