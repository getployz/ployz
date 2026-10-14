import { randomUUID } from "node:crypto";
import { Effect, Result } from "effect";
import { expect, it } from "vitest";
import { GithubDisconnected } from "./github-access";
import { makeGithubTokens } from "./github-token.server";
import { linkGithubInstallation, suspendGithubInstallation, unlinkGithubInstallation, upsertGithubInstallation } from "./github.repository";
import { GithubApi } from "./github-observation.api";
import { listGithubBranches } from "./github.server";
import { SupportStaff, supportStaffOf } from "#/modules/organization/access.server";
import { fakeGithubApi } from "#/test/fake-github";
import { startPostgresTestHarness } from "#/test/postgres";

it("hands an installation token only to an Organization that links an unsuspended installation now, cached or not", async () => {
  const harness = await startPostgresTestHarness();
  try {
    const user = randomUUID(); const shop = randomUUID(); const lab = randomUUID();
    await harness.pool.query('insert into "user"(id,email,name) values($1,$2,$3)', [user, "ada@example.test", "Ada"]);
    await harness.pool.query("insert into organization(id,name,slug) values($1,'Shop','shop'),($2,'Lab','lab')", [shop, lab]);
    await harness.pool.query("insert into github_installation(installation_id,account_login,account_type) values(7,'acme','Organization')");
    for (const organizationId of [shop, lab]) {
      await harness.runEffect(linkGithubInstallation({ organizationId, installationId: 7, connectedByUserId: user, connectedGithubLogin: "ada" }));
    }
    let mints = 0;
    const tokens = await harness.runEffect(makeGithubTokens((installationId) => Effect.sync(() => `token-${installationId}-${(mints += 1)}`)));
    const token = (organizationId: string, installationId: number) => harness.runEffect(Effect.result(tokens.installationToken(organizationId, installationId)));

    expect(await token(shop, 7)).toEqual(Result.succeed("token-7-1"));
    expect(await token(lab, 7)).toEqual(Result.succeed("token-7-1"));
    expect(mints).toBe(1);

    expect(await harness.runEffect(unlinkGithubInstallation({ organizationId: shop, installationId: 7 }))).toBe(true);
    expect(await token(shop, 7)).toEqual(Result.fail(new GithubDisconnected({ organizationId: shop, installationId: 7, accountLogin: "acme", reason: "disconnected" })));
    expect(await token(lab, 7)).toEqual(Result.succeed("token-7-1"));
    expect(mints).toBe(1);

    expect(await token(lab, 9)).toEqual(Result.fail(new GithubDisconnected({ organizationId: lab, installationId: 9, accountLogin: "installation 9", reason: "disconnected" })));
    expect(mints).toBe(1);
    expect(await harness.runEffect(tokens.systemInstallationToken(7))).toBe("token-7-1");

    const started = performance.now();
    const calls = await harness.runEffect(Effect.forEach(
      Array.from({ length: 200 }, () => 7),
      (installationId) => tokens.installationToken(lab, installationId),
    ));
    const perCall = (performance.now() - started) / calls.length;
    expect(calls).toEqual(calls.map(() => "token-7-1"));
    process.stdout.write(`installationToken link check: ${perCall.toFixed(2)} ms per call\n`);
    expect(perCall).toBeLessThan(20);

    const unsuspend = () => harness.runEffect(upsertGithubInstallation({ installationId: 7, accountLogin: "acme", accountType: "Organization", accountAvatarUrl: null }));
    await harness.runEffect(suspendGithubInstallation(7));
    expect(await token(lab, 7)).toEqual(Result.fail(new GithubDisconnected({ organizationId: lab, installationId: 7, accountLogin: "acme", reason: "suspended" })));
    await unsuspend();
    expect(await token(lab, 7)).toEqual(Result.succeed("token-7-2"));

    await harness.runEffect(suspendGithubInstallation(7));
    expect(await harness.runEffect(Effect.result(tokens.systemInstallationToken(7)))).toMatchObject({ _tag: "Failure", failure: { code: "not_found" } });
    await unsuspend();
    expect(await harness.runEffect(tokens.systemInstallationToken(7))).toBe("token-7-3");
  } finally { await harness.stop(); }
}, 60_000);

it("reads a repository GitHub keeps private from the public API as not found", async () => {
  const harness = await startPostgresTestHarness();
  try {
    const user = randomUUID(); const shop = randomUUID();
    await harness.pool.query('insert into "user"(id,email,name) values($1,$2,$3)', [user, "ada@example.test", "Ada"]);
    await harness.pool.query("insert into organization(id,name,slug) values($1,'Shop','shop')", [shop]);
    await harness.pool.query("insert into member(user_id,organization_id,role) values($1,$2,'owner')", [user, shop]);
    const branches = (github: ReturnType<typeof fakeGithubApi>) => harness.runEffect(Effect.result(
      listGithubBranches({ userId: user }, { organizationSlug: "shop", repositoryId: 4243, installationId: null }).pipe(
        Effect.provideService(GithubApi, github.service),
        Effect.provideService(SupportStaff, supportStaffOf([])),
        Effect.mapError((error) => error._tag),
      ),
    ));

    expect(await branches(fakeGithubApi())).toEqual(Result.fail("NotFound"));
    expect(await branches(fakeGithubApi({ "/repositories/4243": "down" }))).toEqual(Result.fail("GithubObservationError"));
  } finally { await harness.stop(); }
}, 60_000);
