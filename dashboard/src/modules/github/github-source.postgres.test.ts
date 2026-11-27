import { access } from "node:fs/promises";
import { gzipSync } from "node:zlib";
import { Header } from "tar";
import { Effect, Exit, Layer, Schema, Scope } from "effect";
import { accessFor } from "./github-access";
import { GithubApi, GithubObservationError } from "./github-observation.api";
import { resolveSourcePaths } from "./github-source.server";
import { GithubSources, GithubSourcesLive } from "./github-sources.server";
import { randomUUID } from "node:crypto";
import { expect, it } from "vitest";
import { getCachedGithubRepositoryForOrganization } from "./github.repository";
import { startPostgresTestHarness } from "#/test/postgres";

function sourceArchive() {
  const blocks = ["root/app/", "root/web/"].map((name) => {
    const header = new Header({ path: name, size: 0, mode: 0o755, type: "Directory" });
    header.encode();
    if (!header.block) throw new Error("Missing archive header");
    return Buffer.from(header.block);
  });
  return new Response(gzipSync(Buffer.concat([...blocks, Buffer.alloc(1024)])));
}

it.each([17, null])("checks out a commit once for every Service on it, with installation %s, and removes it when Cloud stops", async (sourceInstallationId) => {
  const harness = await startPostgresTestHarness();
  try {
    const user = randomUUID(); const organization = randomUUID();
    await harness.pool.query('insert into "user"(id,email,name) values($1,$2,$3)', [user, "source@example.test", "Source"]);
    await harness.pool.query('insert into organization(id,name,slug) values($1,$2,$3)', [organization, "Source", "source"]);
    await harness.pool.query('insert into member(user_id,organization_id) values($1,$2)', [user, organization]);
    await harness.pool.query("insert into github_installation(installation_id,account_login,account_type) values(17,'owner','User')");
    await harness.pool.query("insert into organization_github_installation(organization_id,installation_id,connected_by_user_id,connected_github_login) values($1,17,$2,'owner')", [organization, user]);
    await harness.pool.query("insert into github_repository_cache(installation_id,repository_id,name,full_name,default_branch,private,html_url,repo_updated_at) values(17,42,'repo','owner/repo','main',true,'https://github.com/owner/repo',now())");
    const input = { organizationId: organization, installationId: 17, repositoryId: 42 };
    expect(await harness.runEffect(getCachedGithubRepositoryForOrganization(input))).toEqual({ fullName: "owner/repo" });
    expect(await harness.runEffect(getCachedGithubRepositoryForOrganization({ ...input, organizationId: randomUUID() }))).toBeNull();
    expect(await harness.runEffect(getCachedGithubRepositoryForOrganization({ ...input, installationId: 18 }))).toBeNull();
    const archives: string[] = [];
    const github = Layer.succeed(GithubApi, {
      json: (request) => Schema.decodeUnknownEffect(request.schema)({ id: 42, full_name: "owner/repo" }).pipe(Effect.orDie),
      installationRepositories: () => Effect.die("unused"),
      installationOf: () => Effect.die("unused"),
      archive: (request) => {
        expect(request.access).toEqual(accessFor(organization, sourceInstallationId));
        archives.push(request.sha);
        // GitHub fails the first archive of the second commit once.
        return request.sha === "b".repeat(40) && archives.length === 2
          ? Effect.fail(new GithubObservationError({ operation: "download_source", code: "request_failed", retriable: true, message: "GitHub archive failed." }))
          : Effect.succeed(sourceArchive());
      },
    });
    const pin = (sha: string) => ({ ...input, installationId: sourceInstallationId, sha });
    const cloud = await Effect.runPromise(Scope.make());
    const sources = await harness.runEffect(Layer.buildWithScope(GithubSourcesLive, cloud));
    const checkout = (sha: string) => harness.runEffect(Effect.scoped(Effect.gen(function* () {
      const cache = yield* GithubSources;
      const repositories = yield* Effect.forEach(["/app", "/web", "/app", "/web", "/app"], () => cache.checkout(pin(sha)), { concurrency: "unbounded" });
      for (const repository of repositories) yield* Effect.promise(() => resolveSourcePaths(repository, "/web"));
      const missing = yield* Effect.promise(() => resolveSourcePaths(repositories[0] ?? "", "/missing").then(() => null, (error: Error) => error.message));
      return { repositories: new Set(repositories), missing };
    }).pipe(Effect.provide(Layer.merge(github, Layer.succeedContext(sources))))));
    const first = await checkout("a".repeat(40));
    expect(first.repositories.size).toBe(1);
    expect(first.missing).toBeTruthy();
    expect(await checkout("a".repeat(40))).toEqual(first);
    await expect(checkout("b".repeat(40))).rejects.toThrow();
    await checkout("b".repeat(40));
    expect(archives).toEqual(["a".repeat(40), "b".repeat(40), "b".repeat(40)]);
    await Effect.runPromise(Scope.close(cloud, Exit.void));
    await expect(access([...first.repositories][0] ?? "")).rejects.toThrow();
    await harness.pool.query('delete from organization_github_installation where organization_id=$1', [organization]);
    expect(await harness.runEffect(getCachedGithubRepositoryForOrganization(input))).toBeNull();
  } finally { await harness.stop(); }
}, 60_000);
