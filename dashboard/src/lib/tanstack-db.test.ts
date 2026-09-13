import { describe, expect, it } from "vitest";
import { Schema } from "effect";
import type { VirtualRowProps } from "@tanstack/react-db";
import { parseLiveQueryRow } from "#/lib/tanstack-db";

describe("parseLiveQueryRow", () => {
  it("removes TanStack DB metadata before strict schema parsing", () => {
    const schema = Schema.Struct({ id: Schema.String });
    const liveRow: VirtualRowProps & { id: string } = {
      id: "row-1",
      $synced: true,
      $origin: "remote",
      $key: "row-1",
      $collectionId: "rows",
    };

    expect(parseLiveQueryRow(schema, liveRow)).toEqual({ id: "row-1" });
  });

  it("keeps strict schemas strict for non-TanStack keys", () => {
    const schema = Schema.Struct({ id: Schema.String });
    const liveRow: VirtualRowProps & {
      id: string;
      unexpected: boolean;
    } = {
      id: "row-1",
      unexpected: true,
      $synced: true,
      $origin: "remote",
      $key: "row-1",
      $collectionId: "rows",
    };

    expect(() => parseLiveQueryRow(schema, liveRow)).toThrow(/unexpected/);
  });
});
