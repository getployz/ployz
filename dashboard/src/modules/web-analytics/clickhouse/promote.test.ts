import { expect, it } from "vitest";
import { sameRows } from "#/modules/web-analytics/clickhouse/promote";

const monday = { bucket: new Date("2026-10-12T00:00:00Z"), pageviews: 3, visitors: 2, visits: 2, bounces: 1, totalDuration: 40 };
const tuesday = { bucket: new Date("2026-10-13T00:00:00Z"), pageviews: 1, visitors: 1, visits: 1, bounces: 1, totalDuration: 0 };

it("two stores that return the same rows in different orders agree", () => {
  expect(sameRows([monday, tuesday], [tuesday, monday])).toBe(true);
  expect(sameRows([monday, tuesday], [{ ...tuesday, bucket: tuesday.bucket }, { pageviews: 3, visitors: 2, visits: 2, bounces: 1, totalDuration: 40, bucket: monday.bucket }])).toBe(true);
});

it("a row that differs in one count, or a missing row, breaks agreement", () => {
  expect(sameRows([monday, tuesday], [monday, { ...tuesday, pageviews: 2 }])).toBe(false);
  expect(sameRows([monday, tuesday], [monday])).toBe(false);
});
