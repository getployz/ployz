import type { VirtualRowProps, WithoutVirtualProps } from "@tanstack/react-db";
import type { Schema } from "effect";
import { decodeStrict } from "#/lib/schema";

type PlainLiveQueryRow<Row extends VirtualRowProps> = Row extends VirtualRowProps
  ? WithoutVirtualProps<Row>
  : never;

export function withoutVirtualProps<Row extends VirtualRowProps>(
  row: Row,
): PlainLiveQueryRow<Row> {
  const { $hasPendingWrites, $origin, $key, $collectionId, ...record } = row;
  void $hasPendingWrites;
  void $origin;
  void $key;
  void $collectionId;
  // SAFETY: dropping TanStack's four virtual keys leaves the plain row; destructuring remainder is not WithoutVirtualProps<Row>.
  return record as PlainLiveQueryRow<Row>;
}

export function parseLiveQueryRow<S extends Schema.ConstraintDecoder<object>>(
  schema: S,
  row: VirtualRowProps,
): S["Type"] {
  return decodeStrict(schema, withoutVirtualProps(row));
}
