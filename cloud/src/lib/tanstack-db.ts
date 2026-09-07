import type {
  Collection,
  UtilsRecord,
  VirtualRowProps,
  WithoutVirtualProps,
} from "@tanstack/react-db";
import type { Schema } from "effect";
import { decodeStrict } from "#/modules/environment-design/schema";

type PlainLiveQueryRow<Row extends VirtualRowProps> = Row extends VirtualRowProps
  ? WithoutVirtualProps<Row>
  : never;

export function withoutVirtualProps<Row extends VirtualRowProps>(
  row: Row,
): PlainLiveQueryRow<Row> {
  const { $synced, $origin, $key, $collectionId, ...record } = row;
  void $synced;
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

// TanStack types live-query rows with the four virtual props ($synced,
// $origin, $key, $collectionId), but app-facing collection types use the plain
// row shape. This boundary re-types the collection in one place; the virtual
// props are additive metadata, so reading rows through the plain type is safe.
export function plainRowCollection<
  Row extends VirtualRowProps,
  Utils extends UtilsRecord,
>(
  collection: Collection<Row & object, string | number, Utils>,
): Collection<
  Omit<Row, keyof VirtualRowProps> & object,
  string | number,
  Utils,
  never,
  Omit<Row, keyof VirtualRowProps> & object
> {
  // SAFETY: live-query virtual props are additive metadata; the collection identity is unchanged when those keys are omitted from the row type.
  return collection as typeof collection &
    Collection<
      Omit<Row, keyof VirtualRowProps> & object,
      string | number,
      Utils,
      never,
      Omit<Row, keyof VirtualRowProps> & object
    >;
}
