import { ConfigProvider, Layer } from "effect";
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
