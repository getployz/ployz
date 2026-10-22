import { existsSync, readFileSync } from "node:fs";
import { Effect } from "effect";
import { assertSchemaCompatible } from "../src/server/schema-compatibility.ts";

const databaseUrl = process.env["DATABASE_URL"];
if (!databaseUrl) throw new Error("DATABASE_URL is required to check schema compatibility");
const imageVersion = existsSync(".ployz-version") ? readFileSync(".ployz-version", "utf8").trim() : "development checkout";
await Effect.runPromise(assertSchemaCompatible({ databaseUrl, imageVersion }).pipe(Effect.scoped));
