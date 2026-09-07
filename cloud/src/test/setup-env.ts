import { generateKeyPairSync } from "node:crypto";
import { existsSync } from "node:fs";
import { resolve } from "node:path";
import { loadEnvFile } from "node:process";

const envFile = process.env["PLOYZ_TEST_ENV_FILE"] ?? ".env.test";
const envPath = resolve(process.cwd(), envFile);

process.env["NODE_ENV"] ??= "test";

if (!existsSync(envPath)) {
  throw new Error(`Missing test environment file: ${envPath}`);
}

loadEnvFile(envPath);

if (!process.env["GITHUB_APP_ID"]) {
  process.env["GITHUB_APP_ID"] = "test-app";
}
if (!process.env["GITHUB_APP_SLUG"]) {
  process.env["GITHUB_APP_SLUG"] = "test-app";
}
if (!process.env["GITHUB_APP_WEBHOOK_SECRET"]) {
  process.env["GITHUB_APP_WEBHOOK_SECRET"] = "test-webhook-secret";
}
if (!process.env["GITHUB_APP_PRIVATE_KEY"]) {
  const { privateKey } = generateKeyPairSync("rsa", {
    modulusLength: 2048,
    privateKeyEncoding: { type: "pkcs8", format: "pem" },
    publicKeyEncoding: { type: "spki", format: "pem" },
  });
  process.env["GITHUB_APP_PRIVATE_KEY"] = privateKey;
}
