import { randomUUID } from "node:crypto";
import { expect, it } from "vitest";
import { processGithubInstallationRepositoriesEvent } from "./github.server";
import { startPostgresTestHarness } from "#/test/postgres";

const repository = (id: number, name: string) => ({
  id, name, full_name: `acme/${name}`, private: true, default_branch: "main",
  html_url: `https://github.com/acme/${name}`, updated_at: "2026-10-14T00:00:00Z",
});

it("drops every removed repository from the picker cache and keeps the rest", async () => {
  const harness = await startPostgresTestHarness();
  try {
    const user = randomUUID();
    await harness.pool.query('insert into "user"(id,email,name) values($1,$2,$3)', [user, "removed@example.test", "Removed"]);
    await harness.pool.query("insert into github_installation(user_id,installation_id,account_login,account_type) values($1,7,'acme','Organization')", [user]);
    for (const [id, name] of [[41, "web"], [42, "docs"], [43, "blog"]] as const) {
      await harness.pool.query("insert into github_repository_cache(user_id,installation_id,repository_id,name,full_name,default_branch,private,html_url,repo_updated_at) values($1,7,$2,$3,$4,'main',true,$5,now())",
        [user, id, name, `acme/${name}`, `https://github.com/acme/${name}`]);
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
