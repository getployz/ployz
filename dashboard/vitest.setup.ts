import { onlineManager } from "@tanstack/react-query";

process.env["NODE_ENV"] ??= "test";
process.env["DATABASE_URL"] ??= "postgres://postgres:postgres@localhost:5432/ployz_cloud";
process.env["APP_URL"] ??= "http://localhost:3000";
process.env["BETTER_AUTH_SECRET"] ??= "test-better-auth-secret-1234567890";
process.env["GITHUB_CLIENT_ID"] ??= "test-github-client-id";
process.env["GITHUB_CLIENT_SECRET"] ??= "test-github-client-secret";
process.env["POLAR_ACCESS_TOKEN"] ??= "test-polar-access-token";
process.env["POLAR_SERVER"] ??= "sandbox";
process.env["POLAR_WEBHOOK_SECRET"] ??= "test-polar-webhook-secret";
process.env["POLAR_PRODUCT_ID"] ??= "22222222-2222-4222-8222-222222222222";
// Refuses connections, so no test reaches the real Hosted DNS by accident, whatever the shell exports.
process.env["PLOYZ_HOSTED_DNS_URL"] = "http://127.0.0.1:9/";
process.env["APP_ENCRYPTION_SECRET"] ??= "test-app-encryption-secret-1234567890";

// Backstop for a provider that names its own Hosted DNS URL: a request to the real service fails here.
const REAL_HOSTED_DNS = "dns.ployz.app";
if (!(REAL_HOSTED_DNS in globalThis.fetch)) {
  const realFetch = globalThis.fetch;
  globalThis.fetch = Object.assign((input: RequestInfo | URL, init?: RequestInit) => {
    const url = new URL(input instanceof Request ? input.url : input);
    const host = url.hostname.toLowerCase().replace(/\.$/, "");
    return host === REAL_HOSTED_DNS || host.endsWith(`.${REAL_HOSTED_DNS}`)
      ? Promise.reject(new Error(`Tests must not reach the real Hosted DNS: ${url.href}`))
      : realFetch(input, init);
  }, { [REAL_HOSTED_DNS]: true });
}

// isolate: false lets a jsdom file inherit the previous file's rendered tree; node files have no document.
globalThis.document?.body.replaceChildren();
// Query's online state is a module singleton, so a file that fires `offline` would pause every later file's queries.
onlineManager.setOnline(true);
