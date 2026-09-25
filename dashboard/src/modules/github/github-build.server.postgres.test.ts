import { randomUUID } from "node:crypto";
import { expect, it } from "vitest";
import { startPostgresTestHarness } from "#/test/postgres";
import { listOrganizationGithubRepositories } from "./github-build.server";

const gitSource = (installationId: number, repositoryId: number, repository: string) => ({
  config: { source: { version: 2, type: "git", repository, repositoryId, access: { type: "github-installation", installationId } } },
});

it("lists the GitHub repositories of each Environment's latest Saved State", async () => {
  const harness = await startPostgresTestHarness();
  try {
    const user = randomUUID(); const organization = randomUUID(); const other = randomUUID(); const project = randomUUID();
    const [production, staging] = [randomUUID(), randomUUID()];
    await harness.pool.query('insert into "user"(id,email,name) values($1,$2,$3)', [user, "builds@example.test", "Builds"]);
    await harness.pool.query("insert into organization(id,name,slug) values($1,'Builds','builds'),($2,'Other','other')", [organization, other]);
    await harness.pool.query("insert into project(id,organization_id,name,slug) values($1,$2,'App','app')", [project, organization]);
    for (const [id, namespace] of [[production, "production"], [staging, "staging"]]) {
      await harness.pool.query(
        `insert into environment(id,project_id,organization_id,name,namespace,intent) values($1,$2,$3,$4,$4,'{"version":1,"services":[],"volumes":[]}')`,
        [id, project, organization, namespace],
      );
    }
    const save = (environmentId: string, services: unknown[], at: string) => harness.pool.query(
      `insert into environment_saved_state_snapshot(organization_id,environment_id,actor_id,intent,volume_deletion_authorizations,created_at)
       values($1,$2,$3,$4::jsonb,'[]'::jsonb,$5)`,
      [organization, environmentId, user, JSON.stringify({ services }), at],
    );
    // An older Saved State's repository is gone once a newer one drops it.
    await save(production, [gitSource(7, 1, "acme/old")], "2026-01-01");
    await save(production, [gitSource(7, 42, "acme/api"), { config: { source: { type: "image", image: "nginx" } } }], "2026-01-02");
    await save(staging, [gitSource(7, 42, "acme/api"), gitSource(8, 43, "acme/web")], "2026-01-02");

    expect(await harness.runEffect(listOrganizationGithubRepositories(organization))).toEqual([
      { installationId: 7, repositoryId: 42, fullName: "acme/api" },
      { installationId: 8, repositoryId: 43, fullName: "acme/web" },
    ]);
    expect(await harness.runEffect(listOrganizationGithubRepositories(other))).toEqual([]);
  } finally { await harness.stop(); }
}, 60_000);
