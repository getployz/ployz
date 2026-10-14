import { readFileSync } from "node:fs";
import { Schema } from "effect";

/** The JSON file at `path`, decoded by `schema`; throws when the file doesn't match it. */
export const readJson = <A, I>(path: string | URL, schema: Schema.Codec<A, I>): A =>
  Schema.decodeUnknownSync(Schema.fromJsonString(schema))(readFileSync(path, "utf8"));
