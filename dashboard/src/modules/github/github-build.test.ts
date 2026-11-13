import { Effect } from "effect";
import { describe, expect, it } from "vitest";
import { organizationAccess } from "./github-access";
import { cancelGithubRun, checkGithubBuildRepository, checkGithubBuildWorkflow, findGithubPushRun, rerunGithubRun } from "./github-build.server";
import { GITHUB_RULE_WORKFLOW, type GithubWorkflowTemplate } from "./github-build-workflow";
import { GithubAuthority, type GithubAuthorityService, type GithubBuildRun } from "./github-authority.server";
import { GithubApi, GithubObservationError, githubLacksPermission, type GithubObservationErrorCode } from "./github-observation.api";
import { fakeGithubApi, fakeGithubAuthority } from "#/test/fake-github";

const failure = (code: GithubObservationErrorCode, status: number, retriable = false) =>
  new GithubObservationError({ code, operation: "fetch_workflow", status, retriable, message: "failed" });

/** Run `effect` with `acme/api` (42) in installation 7, the authority answering as `answers` says. */
function withGithub<A, E>(effect: Effect.Effect<A, E, GithubApi | GithubAuthority>, answers: Partial<GithubAuthorityService>) {
  const authority = fakeGithubAuthority(answers);
  const api = fakeGithubApi({ "/repos/acme/api": { id: 42, full_name: "acme/api" } });
  return Effect.runPromise(effect.pipe(
    Effect.provideService(GithubApi, api.service),
    Effect.provideService(GithubAuthority, authority.service),
  )).then((value) => ({ value, calls: authority.calls }));
}

const access = organizationAccess("org", 7);
const workflowFile = (state: string | null, template: GithubWorkflowTemplate | null = "build", dispatchable = template !== null) =>
  ({ fullName: "acme/api", defaultBranch: "main", state, template, dispatchable });

function check(workflow: ReturnType<typeof workflowFile> | GithubObservationError) {
  return withGithub(checkGithubBuildWorkflow(access, 42), {
    buildWorkflow: () => workflow instanceof GithubObservationError ? Effect.fail(workflow) : Effect.succeed(workflow),
  });
}

describe("checkGithubBuildWorkflow", () => {
  it("is ready only when the workflow Ployz offers is active on the default branch", async () => {
    const ready = await check(workflowFile("active"));
    expect(ready.value).toEqual({ fullName: "acme/api", defaultBranch: "main", readiness: "ready", template: "build", dispatchable: true });
    expect(ready.calls).toEqual([{ operation: "buildWorkflow", input: { installationId: 7, repositoryId: 42 } }]);
    expect((await check(workflowFile("disabled_manually"))).value.readiness).toBe("setup_needed");
    expect((await check(workflowFile("deleted"))).value.readiness).toBe("setup_needed");
    expect((await check(workflowFile(null, null))).value.readiness).toBe("setup_needed");
    expect((await check(workflowFile("active", "rule"))).value).toMatchObject({ readiness: "ready", template: "rule" });
  });

  it("reads another active file, such as one calling getployz/build@v1, as outdated, and says whether it still dispatches", async () => {
    expect((await check(workflowFile("active", null))).value).toMatchObject({ readiness: "outdated", template: null, dispatchable: false });
    expect((await check(workflowFile("active", null, true))).value).toMatchObject({ readiness: "outdated", dispatchable: true });
  });

  it("reports an installation without Actions access or without the repository as lacking permission", async () => {
    expect((await check(failure("request_failed", 403))).value.readiness).toBe("no_permission");
    expect((await check(failure("not_found", 404))).value).toMatchObject({ fullName: null, readiness: "no_permission", dispatchable: false });
  });

  it("fails on a rate limit instead of guessing", async () => {
    await expect(check(failure("request_failed", 403, true))).rejects.toThrow();
  });
});

const HEAD = "c".repeat(40);

function pushRuns(runs: GithubBuildRun[], workflow = ".github/workflows/ployz-build.yml") {
  return withGithub(findGithubPushRun({ access, repositoryId: 42, workflow, commit: HEAD }), { buildRuns: () => Effect.succeed(runs) });
}

const run = (id: number, headSha: string, event: string, status = "in_progress"): GithubBuildRun =>
  ({ id, url: `https://github.com/acme/api/actions/runs/${id}`, headSha, event, status, conclusion: null });

describe("checkGithubBuildRepository", () => {
  const contents = (text: string) => ({ content: Buffer.from(text).toString("base64"), encoding: "base64" });
  const row = (ruleBranches: string[], file: ReturnType<typeof workflowFile>, onDisk?: string) => {
    const api = fakeGithubApi(onDisk === undefined ? {} : { "/repositories/42/contents/.github/workflows/ployz-build.yml": contents(onDisk) });
    return Effect.runPromise(checkGithubBuildRepository("org", { installationId: 7, repositoryId: 42, fullName: "acme/api", ruleBranches }).pipe(
      Effect.provideService(GithubApi, api.service),
      Effect.provideService(GithubAuthority, fakeGithubAuthority({ buildWorkflow: () => Effect.succeed(file) }).service),
    ));
  };
  const handWritten = GITHUB_RULE_WORKFLOW.replace("branches: ['**']", "branches: [main]");

  it("offers the build template to a repository no rule builds", async () => {
    expect(await row([], workflowFile("active", "build"))).toMatchObject({ readiness: "ready", template: "build" });
    expect(await row([], workflowFile(null, null))).toMatchObject({ readiness: "setup_needed", template: "build" });
    expect(await row([], workflowFile("active", null), "jobs:\n  build:\n    steps:\n      - uses: getployz/build@v1\n"))
      .toMatchObject({ readiness: "outdated", template: "build" });
  });

  it("offers the rule template to a repository an automatic deploy rule builds, never the build template", async () => {
    expect(await row(["main"], workflowFile("active", "rule"))).toMatchObject({ readiness: "ready", template: "rule", ruleBranch: "main" });
    expect(await row(["main", "release"], workflowFile("active", "rule"))).toMatchObject({ template: "rule", ruleBranch: null });
    expect(await row(["main"], workflowFile("active", "build"))).toMatchObject({ readiness: "outdated", template: "rule" });
    expect(await row(["main"], workflowFile(null, null))).toMatchObject({ readiness: "setup_needed", template: "rule" });
    expect(await row(["main"], workflowFile("active", null), handWritten)).toMatchObject({ readiness: "outdated", template: "rule" });
  });

  it("offers the rule template where the file calls a reusable workflow or already is the rule template", async () => {
    expect(await row([], workflowFile("active", null), handWritten)).toMatchObject({ readiness: "outdated", template: "rule" });
    expect(await row([], workflowFile("active", "rule"))).toMatchObject({ readiness: "ready", template: "rule" });
  });

  it("offers the build template when the file can't be read", async () => {
    expect(await row([], workflowFile("active", null))).toMatchObject({ readiness: "outdated", template: "build" });
  });
});

describe("findGithubPushRun", () => {
  it("follows the run the push of the commit started", async () => {
    const found = await pushRuns([run(1, HEAD, "workflow_dispatch"), run(2, "d".repeat(40), "push"), run(3, HEAD, "push")]);
    expect(found.value).toEqual({ runId: 3, runUrl: "https://github.com/acme/api/actions/runs/3", completed: false });
    expect(found.calls).toEqual([{ operation: "buildRuns", input: { installationId: 7, repositoryId: 42, headSha: HEAD, event: "push" } }]);
  });

  it("says when the push run already completed, so Cloud runs it again", async () => {
    expect((await pushRuns([run(3, HEAD, "push", "completed")])).value).toMatchObject({ runId: 3, completed: true });
  });

  it("finds nothing until GitHub lists a push run of the commit", async () => {
    expect((await pushRuns([])).value).toBeNull();
    expect((await pushRuns([run(1, HEAD, "workflow_dispatch")])).value).toBeNull();
  });

  it("follows no workflow but Ployz's build workflow", async () => {
    await expect(pushRuns([run(3, HEAD, "push")], ".github/workflows/release.yml")).rejects.toThrow(/ployz-build.yml/);
  });
});

describe("a build run Cloud acts on", () => {
  it("is run again through the authority", async () => {
    const rerun = await withGithub(rerunGithubRun({ access, repositoryId: 42, runId: 3 }), { rerunBuild: () => Effect.void });
    expect(rerun.calls).toEqual([{ operation: "rerunBuild", input: { installationId: 7, repositoryId: 42, runId: 3 } }]);
  });

  it("is cancelled in the repository its recorded name resolves to", async () => {
    const cancel = await withGithub(cancelGithubRun({ access, fullName: "acme/api", runId: 3 }), { cancelBuild: () => Effect.void });
    expect(cancel.calls).toEqual([{ operation: "cancelBuild", input: { installationId: 7, repositoryId: 42, runId: 3 } }]);
  });
});

describe("an installation that never accepted a permission an operation needs", () => {
  it("lacks it when GitHub refuses the operation's token with 422, so builds read no_permission and the PR check is skipped", async () => {
    const refused = new GithubObservationError({ code: "request_failed", operation: "installation_token", status: 422, retriable: false, message: "not granted" });
    expect(githubLacksPermission(refused)).toBe(true);
    expect(githubLacksPermission(new GithubObservationError({ ...refused, operation: "create_check_run" }))).toBe(false);
    expect((await check(refused)).value).toMatchObject({ fullName: null, readiness: "no_permission" });
  });
});
