import { Effect, Schema } from "effect";
import { describe, expect, it } from "vitest";
import { organizationAccess } from "./github-access";
import { checkGithubBuildWorkflow, findGithubPushRun, rerunGithubRun } from "./github-build.server";
import { GITHUB_BUILD_WORKFLOW, githubBuildId, githubBuildWorkflowUrl } from "./github-build-workflow";
import { GithubApi, GithubObservationError, type GithubObservationErrorCode } from "./github-observation.api";

describe("GitHub build workflow", () => {
  it("calls getployz/build@v1 with every input Cloud dispatches", () => {
    expect(GITHUB_BUILD_WORKFLOW).toContain("uses: getployz/build@v1");
    for (const input of ["build", "cloud"]) {
      expect(GITHUB_BUILD_WORKFLOW).toContain(`${input}: \${{ inputs.${input} }}`);
    }
  });

  it("dispatches build ids that getployz/build@v1 accepts", () => {
    const accepted = /^[A-Za-z0-9_-][A-Za-z0-9._-]{0,127}$/;
    const deployment = "36755d9a-dc30-4385-a9bd-86ae1a85e44b";
    for (const service of ["a", "ployz-soak-node", "s".repeat(63)]) {
      expect(githubBuildId(deployment, service)).toMatch(accepted);
    }
  });

  it("opens GitHub's new-file page on the default branch with the workflow filled in", () => {
    const url = new URL(githubBuildWorkflowUrl({ web: "http://127.0.0.1:4555", fullName: "acme/api", defaultBranch: "release/v2" }));
    expect(url.origin + url.pathname).toBe("http://127.0.0.1:4555/acme/api/new/release/v2");
    expect(url.searchParams.get("filename")).toBe(".github/workflows/ployz-build.yml");
    expect(url.searchParams.get("value")).toBe(GITHUB_BUILD_WORKFLOW);
  });
});

type Reply = { body: unknown } | { error: GithubObservationErrorCode; status: number; retriable?: boolean };

function check(replies: { repository: Reply; workflow?: Reply }) {
  const urls: string[] = [];
  return Effect.runPromise(checkGithubBuildWorkflow(organizationAccess("org", 7), 42).pipe(
    Effect.provideService(GithubApi, {
      archive: () => Effect.die("unused"),
      app: () => Effect.die("unused"),
      installationRepositories: () => Effect.die("unused"),
      exchangeUserCode: () => Effect.die("unused"),
      userJson: () => Effect.die("unused"),
      json: (request) => {
        urls.push(request.path);
        const reply = request.path.endsWith("/repositories/42") ? replies.repository : replies.workflow;
        if (!reply) return Effect.die(`unexpected ${request.path}`);
        if ("error" in reply) {
          return Effect.fail(new GithubObservationError({
            code: reply.error, operation: request.operation, status: reply.status, retriable: reply.retriable ?? false, message: "failed",
          }));
        }
        return Schema.decodeUnknownEffect(request.schema)(reply.body).pipe(Effect.orDie);
      },
    }),
  )).then((result) => ({ ...result, urls }));
}

const repository = { body: { id: 42, full_name: "acme/api", default_branch: "main" } };
const workflow = (state: string) => ({ body: { path: ".github/workflows/ployz-build.yml", state } });

describe("checkGithubBuildWorkflow", () => {
  it("is ready only when the workflow is active on the default branch", async () => {
    const ready = await check({ repository, workflow: workflow("active") });
    expect(ready).toMatchObject({ fullName: "acme/api", defaultBranch: "main", readiness: "ready" });
    expect(ready.urls[1]).toBe("/repos/acme/api/actions/workflows/ployz-build.yml");
    expect((await check({ repository, workflow: workflow("disabled_manually") })).readiness).toBe("setup_needed");
    expect((await check({ repository, workflow: workflow("deleted") })).readiness).toBe("setup_needed");
    expect((await check({ repository, workflow: { error: "not_found", status: 404 } })).readiness).toBe("setup_needed");
  });

  it("reports an installation without Actions access or without the repository as lacking permission", async () => {
    expect((await check({ repository, workflow: { error: "request_failed", status: 403 } })).readiness).toBe("no_permission");
    expect(await check({ repository: { error: "not_found", status: 404 } })).toMatchObject({ fullName: null, readiness: "no_permission" });
  });

  it("fails on a rate limit instead of guessing", async () => {
    await expect(check({ repository, workflow: { error: "request_failed", status: 403, retriable: true } })).rejects.toThrow();
  });
});

const HEAD = "c".repeat(40);

function pushRuns(runs: unknown[]) {
  const paths: string[] = [];
  return Effect.runPromise(findGithubPushRun({
    access: organizationAccess("org", 7), fullName: "acme/api", workflow: ".github/workflows/ployz-build.yml", commit: HEAD,
  }).pipe(
    Effect.provideService(GithubApi, {
      archive: () => Effect.die("unused"),
      app: () => Effect.die("unused"),
      installationRepositories: () => Effect.die("unused"),
      exchangeUserCode: () => Effect.die("unused"),
      userJson: () => Effect.die("unused"),
      json: (request) => {
        paths.push(request.path);
        return Schema.decodeUnknownEffect(request.schema)({ workflow_runs: runs }).pipe(Effect.orDie);
      },
    }),
  )).then((run) => ({ run, paths }));
}

const run = (id: number, headSha: string, event: string, status = "in_progress") =>
  ({ id, html_url: `https://github.com/acme/api/actions/runs/${id}`, head_sha: headSha, event, status });

describe("findGithubPushRun", () => {
  it("follows the run the push of the commit started", async () => {
    const found = await pushRuns([run(1, HEAD, "workflow_dispatch"), run(2, "d".repeat(40), "push"), run(3, HEAD, "push")]);
    expect(found.run).toEqual({ runId: 3, runUrl: "https://github.com/acme/api/actions/runs/3", completed: false });
    expect(found.paths).toEqual([`/repos/acme/api/actions/workflows/ployz-build.yml/runs?head_sha=${HEAD}&event=push&per_page=5`]);
  });

  it("says when the push run already completed, so Cloud runs it again", async () => {
    expect((await pushRuns([run(3, HEAD, "push", "completed")])).run).toMatchObject({ runId: 3, completed: true });
  });

  it("finds nothing until GitHub lists a push run of the commit", async () => {
    expect((await pushRuns([])).run).toBeNull();
    expect((await pushRuns([run(1, HEAD, "workflow_dispatch")])).run).toBeNull();
  });
});

describe("rerunGithubRun", () => {
  it("asks GitHub to run the run again", async () => {
    const requests: { path: string; method: string | undefined }[] = [];
    await Effect.runPromise(rerunGithubRun({ access: organizationAccess("org", 7), fullName: "acme/api", runId: 3 }).pipe(
      Effect.provideService(GithubApi, {
        archive: () => Effect.die("unused"),
        app: () => Effect.die("unused"),
        installationRepositories: () => Effect.die("unused"),
        exchangeUserCode: () => Effect.die("unused"),
        userJson: () => Effect.die("unused"),
        json: (request) => {
          requests.push({ path: request.path, method: request.method });
          return Schema.decodeUnknownEffect(request.schema)({}).pipe(Effect.orDie);
        },
      }),
    ));
    expect(requests).toEqual([{ path: "/repos/acme/api/actions/runs/3/rerun", method: "POST" }]);
  });
});
