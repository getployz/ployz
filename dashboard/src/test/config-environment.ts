import { ConfigProvider, Layer } from "effect";
import { afterAll } from "vitest";
import { AppConfig } from "#/server/config.server";
import { TEST_SERVICE_VARIABLES } from "#/test/test-env";

function serviceVariable(name: string) {
  const value = process.env[name];
  if (value === undefined) throw new Error(`Missing test variable ${name}`);
  return [name, value] as const;
}

/** Every variable AppConfig requires; spread first, then override what a test cares about.
 * Service credentials come from `setup-env.ts` and `.env.test`. */
export function testConfigEnvironment() {
  return {
    APP_URL: "http://localhost:3000",
    BETTER_AUTH_SECRET: "better-auth-secret",
    GITHUB_CLIENT_ID: "github-client-id",
    GITHUB_CLIENT_SECRET: "github-client-secret",
    APP_ENCRYPTION_SECRET: "app-encryption-secret-at-least-32-characters",
    // A closed port, never the real Hosted DNS: a test that reserves a Cluster Domain starts a fake one.
    PLOYZ_HOSTED_DNS_URL: "http://127.0.0.1:9/",
    ...Object.fromEntries(TEST_SERVICE_VARIABLES.map(serviceVariable)),
  };
}

export const testConfigLayer = (overrides: Readonly<Record<string, string>> = {}) =>
  AppConfig.layer.pipe(Layer.provide(ConfigProvider.layer(ConfigProvider.fromEnv({ env: { ...testConfigEnvironment(), ...overrides } }))));

const POLAR_VARIABLES = ["POLAR_ACCESS_TOKEN", "POLAR_SERVER", "POLAR_WEBHOOK_SECRET", "POLAR_PRODUCT_ID"] as const;

/** Runs the calling test file as a Self-hosted Cloud, one with no Polar settings, through the app's own runtime.
 * Call it at module load: the runtime reads the settings when the file's first request builds it. */
export function runAsSelfHostedCloud() {
  const saved = POLAR_VARIABLES.map((name) => [name, process.env[name]] as const);
  for (const name of POLAR_VARIABLES) delete process.env[name];
  afterAll(() => {
    for (const [name, value] of saved) if (value !== undefined) process.env[name] = value;
  });
}
