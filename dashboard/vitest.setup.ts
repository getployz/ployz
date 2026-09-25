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
// Refuses connections, so no test reaches the real Hosted DNS by accident.
process.env["PLOYZ_HOSTED_DNS_URL"] ??= "http://127.0.0.1:9/";
process.env["APP_ENCRYPTION_SECRET"] ??= "test-app-encryption-secret-1234567890";
