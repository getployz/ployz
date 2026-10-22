import { existsSync, readFileSync } from "node:fs";
import { Effect } from "effect";
import { assertSchemaCompatible } from "../src/server/schema-compatibility.ts";

const databaseUrl = process.env["DATABASE_URL"];
if (!databaseUrl) throw new Error("DATABASE_URL is required to check schema compatibility");
const imageVersion = existsSync(".ployz-version") ? readFileSync(".ployz-version", "utf8").trim() : "development checkout";
// A rollback to an older image must stay possible; the operator takes on the requests it may fail.
const allowOlderImage = process.env["PLOYZ_ALLOW_OLDER_IMAGE"] === "1";
await Effect.runPromise(assertSchemaCompatible({ databaseUrl, imageVersion }).pipe(
  Effect.catchTag("UnsupportedDatabaseSchema", (error) => allowOlderImage
    ? Effect.sync(() => console.warn(`Starting anyway because PLOYZ_ALLOW_OLDER_IMAGE=1. ${error.message}`))
    : Effect.fail(error)),
  Effect.scoped,
));
