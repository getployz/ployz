import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";

const url = process.env["OBSERVE_CLICKHOUSE_URL"];
if (!url) {
  console.log("OBSERVE_CLICKHOUSE_URL is not set; skipping the observe ClickHouse migration.");
  process.exit(0);
}

const user = process.env["CLICKHOUSE_USER"] ?? "default";
const password = process.env["CLICKHOUSE_PASSWORD"] ?? "";
const dir = process.env["OBSERVE_DDL_DIR"] ?? "observe/clickhouse";
const authorization = `Basic ${Buffer.from(`${user}:${password}`).toString("base64")}`;

async function run(statement: string) {
  const response = await fetch(url!, { method: "POST", headers: { authorization }, body: statement });
  if (!response.ok) throw new Error(`ClickHouse refused a statement (${response.status}): ${(await response.text()).trim()}\n${statement}`);
}

await run("CREATE DATABASE IF NOT EXISTS otel");
const files = readdirSync(dir).filter((name) => name.endsWith(".sql")).sort();
for (const file of files) {
  // A line holding only `;` ends a statement, so a statement may hold semicolons of its own.
  const statements = readFileSync(join(dir, file), "utf8").split(/^;$/m).map((s) => s.trim()).filter((s) => s !== "");
  for (const statement of statements) await run(statement);
  console.log(`applied ${file} (${statements.length} statements)`);
}
