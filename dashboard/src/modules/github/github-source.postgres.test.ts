import { access } from "node:fs/promises";
import { gzipSync } from "node:zlib";
import { Header } from "tar";
import { Effect, Schema } from "effect";
import { GithubApi } from "./github-observation.api";
import { materializeGithubSource } from "./github-source.server";
import { randomUUID } from "node:crypto";
import { expect, it } from "vitest";
import { getCachedGithubRepositoryForOrganization } from "./github.repository";
import { startPostgresTestHarness } from "#/test/postgres";

it.each([17, null])("materializes pinned source with installation %s and cleans it up after failure", async (sourceInstallationId) => {
  const harness = await startPostgresTestHarness();
  try {
    const user = randomUUID(); const organization = randomUUID();
    await harness.pool.query('insert into "user"(id,email,name) values($1,$2,$3)', [user, "source@example.test", "Source"]);
    await harness.pool.query('insert into organization(id,name,slug) values($1,$2,$3)', [organization, "Source", "source"]);
    await harness.pool.query('insert into member(user_id,organization_id) values($1,$2)', [user, organization]);
    await harness.pool.query("insert into github_installation(user_id,installation_id,account_login,account_type) values($1,17,'owner','User')", [user]);
    await harness.pool.query("insert into github_repository_cache(user_id,installation_id,repository_id,name,full_name,default_branch,private,html_url,repo_updated_at) values($1,17,42,'repo','owner/repo','main',true,'https://github.com/owner/repo',now())", [user]);
    const input = { organizationId: organization, installationId: 17, repositoryId: 42 };
    expect(await harness.runEffect(getCachedGithubRepositoryForOrganization(input))).toEqual({ fullName: "owner/repo" });
    expect(await harness.runEffect(getCachedGithubRepositoryForOrganization({ ...input, organizationId: randomUUID() }))).toBeNull();
    expect(await harness.runEffect(getCachedGithubRepositoryForOrganization({ ...input, installationId: 18 }))).toBeNull();
    const header = new Header({ path: "root/Dockerfile", size: 0, mode: 0o644, type: "File" });
    header.encode();
    if (!header.block) throw new Error("Missing archive header");
    const response = new Response(gzipSync(Buffer.concat([Buffer.from(header.block), Buffer.alloc(1024)])));
    let checkout: string | undefined;
    await harness.runEffect(Effect.scoped(materializeGithubSource({ ...input, installationId: sourceInstallationId, sha: "a".repeat(40), rootDir: "." }).pipe(
      Effect.flatMap((source) => {
        checkout = source.repositoryDirectory;
        return Effect.fail("Build failed after acquisition");
      }),
      Effect.provideService(GithubApi, {
        json: (request) => Schema.decodeUnknownEffect(request.schema)({ id: 42, full_name: "owner/repo" }).pipe(Effect.orDie),
        archive: (request) => {
          expect(request.installationId).toBe(sourceInstallationId);
          expect(request.sha).toBe("a".repeat(40));
          return Effect.succeed(response);
        },
      }),
    )).pipe(Effect.result));
    expect(checkout).toBeDefined();
    if (!checkout) throw new Error("Source was not acquired");
    await expect(access(checkout)).rejects.toThrow();
    await harness.pool.query('delete from member where user_id=$1', [user]);
    expect(await harness.runEffect(getCachedGithubRepositoryForOrganization(input))).toBeNull();
  } finally { await harness.stop(); }
}, 60_000);
