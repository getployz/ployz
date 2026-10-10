import { expect, it } from "vitest";
import { Option, Schema } from "effect";
import { logHistorySchema } from "./container-logs.server";

it("accepts a history cursor holding a place on each of 50 Servers", () => {
  const place = { c: `1760000000000000000:${"a".repeat(64)}:12:34`, t: "1760000000000000000", n: 3 };
  const cursor = Buffer.from(JSON.stringify(Object.fromEntries(Array.from({ length: 50 }, (_, index) => [`${index}`.padStart(32, "0"), place])))).toString("base64url");
  const search = Schema.decodeUnknownOption(logHistorySchema)({ organizationSlug: "acme", environmentSlug: "production", projectSlug: "web", cursor });
  expect(Option.getOrUndefined(search)?.cursor).toBe(cursor);
});
