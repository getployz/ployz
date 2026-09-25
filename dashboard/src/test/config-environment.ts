const SERVICE_VARIABLES = [
  "GITHUB_APP_ID",
  "GITHUB_APP_PRIVATE_KEY",
  "GITHUB_APP_SLUG",
  "GITHUB_APP_WEBHOOK_SECRET",
  "INNGEST_EVENT_KEY",
  "INNGEST_SIGNING_KEY",
] as const;

function serviceVariable(name: (typeof SERVICE_VARIABLES)[number]) {
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
    ...Object.fromEntries(SERVICE_VARIABLES.map(serviceVariable)),
  };
}
