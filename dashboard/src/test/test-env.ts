import { generateKeyPairSync } from "node:crypto";

/** Service credentials a developer's `.env.test` need not set. A test sets what it cares about on top. */
export const TEST_SERVICE_DEFAULTS = {
  GITHUB_APP_ID: () => "test-app",
  GITHUB_APP_CLIENT_ID: () => "test-app-client",
  GITHUB_APP_CLIENT_SECRET: () => "test-app-client-secret",
  GITHUB_APP_SLUG: () => "test-app",
  GITHUB_APP_WEBHOOK_SECRET: () => "test-webhook-secret",
  GITHUB_APP_PRIVATE_KEY: () => generateKeyPairSync("rsa", {
    modulusLength: 2048,
    privateKeyEncoding: { type: "pkcs8", format: "pem" },
    publicKeyEncoding: { type: "spki", format: "pem" },
  }).privateKey,
} as const;

/** Credentials with no default; `.env.test` provides them. */
export const TEST_ENV_FILE_VARIABLES = ["INNGEST_EVENT_KEY", "INNGEST_SIGNING_KEY"] as const;

export const TEST_SERVICE_VARIABLES: readonly string[] = [...Object.keys(TEST_SERVICE_DEFAULTS), ...TEST_ENV_FILE_VARIABLES].sort();
