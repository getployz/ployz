// The fake Hosted DNS the verify dashboard reserves Cluster Domains from: grants `<org slug>.ployz.test` and signs
// certificates with a throwaway CA. up.sh runs it with runner.mjs until down.sh stops it. Prints
// `VERIFY_HOSTED_DNS <url>` once listening, then one line per request it answers.
import { startFakeHostedDns } from "#/modules/cluster-domain/hosted-dns.test-fixture";

export async function run() {
  const dns = await startFakeHostedDns();
  console.log(`VERIFY_HOSTED_DNS ${dns.url}`);
  let logged = 0;
  setInterval(() => {
    for (const { method, path } of dns.requests.slice(logged)) console.log(`${new Date().toISOString()} ${method} ${path}`);
    logged = dns.requests.length;
  }, 500);
  await new Promise<never>(() => {});
}
