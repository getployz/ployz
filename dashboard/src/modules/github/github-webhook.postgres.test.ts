import { expect, it } from "vitest";
import { processGithubInstallationEvent, processGithubInstallationRepositoriesEvent } from "./github.server";
import { linkGithubInstallation } from "./github.repository";
import { startPostgresTestHarness } from "#/test/postgres";

const installation = (action: "suspend" | "unsuspend" | "deleted") => ({
  deliveryId: `delivery-${action}`, action, sender: { id: 9001, login: "ada" },
  installation: { id: 7, account: { login: "acme", type: "Organization", avatar_url: "https://avatars.githubusercontent.com/u/7" } },
});

const repository = (id: number, name: string) => ({
  id, name, full_name: `acme/${name}`, private: true, default_branch: "main",
  html_url: `https://github.com/acme/${name}`, updated_at: "2026-10-14T00:00:00Z",
});

it("drops every removed repository from the picker cache and keeps the rest", async () => {
  const harness = await startPostgresTestHarness();
  try {
    await harness.pool.query("insert into github_installation(installation_id,account_login,account_type) values(7,'acme','Organization')");
    for (const [id, name] of [[41, "web"], [42, "docs"], [43, "blog"]] as const) {
      await harness.pool.query("insert into github_repository_cache(installation_id,repository_id,name,full_name,default_branch,private,html_url,repo_updated_at) values(7,$1,$2,$3,'main',true,$4,now())",
        [id, name, `acme/${name}`, `https://github.com/acme/${name}`]);
    }

    await harness.runEffect(processGithubInstallationRepositoriesEvent({
      deliveryId: "delivery-1", action: "removed", installation: { id: 7 },
      repositories_added: [], repositories_removed: [repository(41, "web"), repository(42, "docs")],
      sender: { id: 9001, login: "ada" },
    }));

    const { rows } = await harness.pool.query<{ full_name: string }>("select full_name from github_repository_cache where installation_id=7");
    expect(rows.map((row) => row.full_name)).toEqual(["acme/blog"]);
  } finally { await harness.stop(); }
}, 60_000);

it("suspend marks the installation suspended, empties the cache and keeps its links; deleted forgets all three; both name the linked Organizations", async () => {
  const harness = await startPostgresTestHarness();
  try {
    const shop = "00000000-0000-4000-8000-000000000001";
    const user = "00000000-0000-4000-8000-000000000101";
    await harness.pool.query('insert into "user"(id,email,name) values($1,$2,$3)', [user, "ada@example.test", "Ada"]);
    await harness.pool.query("insert into organization(id,name,slug) values($1,'Shop','shop')", [shop]);
    await harness.pool.query("insert into github_installation(installation_id,account_login,account_type) values(7,'acme','Organization')");
    await harness.pool.query("insert into github_repository_cache(installation_id,repository_id,name,full_name,default_branch,private,html_url,repo_updated_at) values(7,41,'web','acme/web','main',true,'https://github.com/acme/web',now())");
    await harness.runEffect(linkGithubInstallation({ organizationId: shop, installationId: 7, connectedByUserId: user, connectedGithubLogin: "ada" }));
    const counts = async () => {
      const { rows } = await harness.pool.query<{ installations: string; suspended: string; cached: string; links: string }>(
        "select (select count(*) from github_installation where installation_id=7) installations, (select count(*) from github_installation where installation_id=7 and suspended_at is not null) suspended, (select count(*) from github_repository_cache where installation_id=7) cached, (select count(*) from organization_github_installation where installation_id=7) links");
      return rows[0];
    };

    expect(await harness.runEffect(processGithubInstallationEvent(installation("suspend")))).toEqual({ installationId: 7, shouldSyncRepositories: false, stopBuildsFor: [shop] });
    expect(await counts()).toEqual({ installations: "1", suspended: "1", cached: "0", links: "1" });

    expect(await harness.runEffect(processGithubInstallationEvent(installation("unsuspend")))).toEqual({ installationId: 7, shouldSyncRepositories: true, stopBuildsFor: [] });
    expect(await counts()).toEqual({ installations: "1", suspended: "0", cached: "0", links: "1" });

    expect(await harness.runEffect(processGithubInstallationEvent(installation("deleted"))))
      .toEqual({ installationId: 7, shouldSyncRepositories: false, stopBuildsFor: [shop] });
    expect(await counts()).toEqual({ installations: "0", suspended: "0", cached: "0", links: "0" });
  } finally { await harness.stop(); }
}, 60_000);
