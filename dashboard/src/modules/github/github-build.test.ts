import { Effect, Schema } from "effect";
import { describe, expect, it } from "vitest";
import { checkGithubBuildWorkflow } from "./github-build.server";
import { GITHUB_BUILD_WORKFLOW, githubBuildWorkflowUrl } from "./github-build-workflow";
import { GithubApi, GithubObservationError, type GithubObservationErrorCode } from "./github-observation.api";

describe("GitHub build workflow", () => {
  it("calls getployz/build@v1 with every input Cloud dispatches", () => {
    expect(GITHUB_BUILD_WORKFLOW).toContain("uses: getployz/build@v1");
    for (const input of ["build", "cloud", "ployz_version"]) {
      expect(GITHUB_BUILD_WORKFLOW).toContain(`${input}: \${{ inputs.${input} }}`);
    }
  });

  it("opens GitHub's new-file page on the default branch with the workflow filled in", () => {
    const url = new URL(githubBuildWorkflowUrl({ fullName: "acme/api", defaultBranch: "release/v2" }));
    expect(url.origin + url.pathname).toBe("https://github.com/acme/api/new/release/v2");
    expect(url.searchParams.get("filename")).toBe(".github/workflows/ployz-build.yml");
    expect(url.searchParams.get("value")).toBe(GITHUB_BUILD_WORKFLOW);
  });
});

type Reply = { body: unknown } | { error: GithubObservationErrorCode; status: number; retriable?: boolean };

function check(replies: { repository: Reply; workflow?: Reply }) {
  const urls: string[] = [];
  return Effect.runPromise(checkGithubBuildWorkflow(7, 42).pipe(
    Effect.provideService(GithubApi, {
      archive: () => Effect.die("unused"),
      json: (request) => {
        urls.push(request.url);
        const reply = request.url.endsWith("/repositories/42") ? replies.repository : replies.workflow;
        if (!reply) return Effect.die(`unexpected ${request.url}`);
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
    expect(ready.urls[1]).toBe("https://api.github.com/repos/acme/api/actions/workflows/ployz-build.yml");
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
