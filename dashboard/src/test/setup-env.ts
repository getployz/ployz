import { existsSync } from "node:fs";
import { resolve } from "node:path";
import { loadEnvFile } from "node:process";
import { TEST_SERVICE_DEFAULTS } from "./test-env";

const envFile = process.env["PLOYZ_TEST_ENV_FILE"] ?? ".env.test";
const envPath = resolve(process.cwd(), envFile);

process.env["NODE_ENV"] ??= "test";

if (!existsSync(envPath)) {
  throw new Error(`Missing test environment file: ${envPath}`);
}

loadEnvFile(envPath);

for (const [name, defaultValue] of Object.entries(TEST_SERVICE_DEFAULTS)) {
  process.env[name] ||= defaultValue();
}
