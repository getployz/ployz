import { describe, expect, it } from "vitest";
import { githubSkipReason, type GithubImageBuild } from "./image-build";

type Report = NonNullable<GithubImageBuild["report"]>;
const report = (patch: Partial<Report> = {}, stepFailed = false): Report => ({
  received: 3, collector: { build: 0, open: null, stepFailed, rows: [] }, platforms: null, ...patch,
});

describe("githubSkipReason", () => {
  it("keeps a failed Build Step final, whether or not the final report arrived or time ran out", () => {
    expect(githubSkipReason(report({}, true), false)).toBeNull();
    expect(githubSkipReason(report({ platforms: [] }, true), false)).toBeNull();
    expect(githubSkipReason(report({}, true), true)).toBeNull();
  });

  it("moves on when the runner stopped before its final report", () => {
    expect(githubSkipReason(null, false)).toEqual({ builder: "github", kind: "runner_stopped" });
    expect(githubSkipReason(report(), false)).toEqual({ builder: "github", kind: "runner_stopped" });
  });

  it("moves on when the final report pushed nothing", () => {
    expect(githubSkipReason(report({ platforms: [] }), false)).toEqual({ builder: "github", kind: "no_push" });
    expect(githubSkipReason(report({ platforms: ["linux/amd64"] }), false)).toEqual({ builder: "github", kind: "no_push" });
  });

  it("moves on, naming the version, when ployz couldn't be installed", () => {
    expect(githubSkipReason(report({ platforms: [], installFailed: "0.1.0-beta.28" }), false))
      .toEqual({ builder: "github", kind: "install_failed", version: "0.1.0-beta.28" });
  });

  it("moves on when the run ran out of time", () => {
    expect(githubSkipReason(report(), true)).toEqual({ builder: "github", kind: "out_of_time" });
  });
});
