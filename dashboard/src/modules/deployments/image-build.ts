import type { BuildGrantId } from "@ployz/sdk";
import { Schema } from "effect";
import { rustMachineIdSchema } from "#/modules/machines/enrollment";
import { collectorCheckpointSchema } from "./preparation-progress";

/**
 * Why a Builder is in an Image Build's walk: the Service prefers it, or the Build Order puts it
 * first or next. GitHub records it when it takes the build; a Server's reason comes from the Engine.
 */
export const CANDIDATE_REASONS = ["preferred", "first_in_build_order", "next_in_build_order"] as const;
export type CandidateReason = (typeof CANDIDATE_REASONS)[number];

/** Why a Builder didn't take an Image Build: one entry of its skip trail, in walk order. */
export const skipReasonSchema = Schema.Union([
  Schema.Struct({ builder: Schema.Literal("github"), kind: Schema.Literal("not_connected") }),
  Schema.Struct({ builder: Schema.Literal("github"), kind: Schema.Literals(["no_permission", "no_workflow"]), repository: Schema.String }),
  Schema.Struct({ builder: Schema.Literal("github"), kind: Schema.Literal("multi_platform"), platforms: Schema.Array(Schema.String) }),
  Schema.Struct({ builder: Schema.Literal("github"), kind: Schema.Literal("dispatch_failed"), message: Schema.String }),
  Schema.Struct({ builder: Schema.Literal("github"), kind: Schema.Literal("ended_before_start") }),
  /**
   * A started run failed for GitHub's reasons, not the build's: it ended without a final report
   * (`runner_stopped`), reported no failed Build Step but pushed nothing (`no_push`), or used up its
   * budget (`out_of_time`). A failed Build Step is final and never a skip.
   */
  Schema.Struct({ builder: Schema.Literal("github"), kind: Schema.Literals(["runner_stopped", "no_push", "out_of_time"]) }),
  Schema.Struct({ builder: Schema.Literal("github"), kind: Schema.Literal("install_failed"), version: Schema.String }),
  /** The Service's Preferred Server is gone (`name` null) or no longer accepts builds: the walk went back to Auto. */
  Schema.Struct({ builder: Schema.Literal("servers"), kind: Schema.Literal("preferred_unavailable"), machineId: rustMachineIdSchema, name: Schema.NullOr(Schema.String) }),
  /** It hadn't started the build within its "start within" limit. */
  Schema.Struct({ builder: Schema.Literal("github"), kind: Schema.Literal("not_started"), minutes: Schema.Number }),
  /** No Server started it in time; `machineName` is the Server it waited on, once the Engine chose one. */
  Schema.Struct({ builder: Schema.Literal("servers"), kind: Schema.Literal("not_started"), minutes: Schema.Number, machineName: Schema.optionalKey(Schema.String) }),
]);
export type SkipReason = typeof skipReasonSchema.Type;

/**
 * A Preferred Server that can't build, whether Cloud found it when planning the walk (a skip) or the
 * Engine did when choosing (its reason): the same words either way. `name` is null once it left the Cluster.
 */
export const preferredServerUnavailableText = (name: string | null) =>
  name === null ? "Your preferred server left the Cluster." : `Your preferred server ${name} is offline or no longer builds.`;

/**
 * Why a Builder didn't take an Image Build, as one plain sentence: the build log says it before
 * "Building on <next> instead.", and a build no Builder took fails with it.
 */
export function skipReasonText(reason: SkipReason): string {
  switch (reason.kind) {
    case "not_connected": return "GitHub can't reach this repository: it isn't connected through the GitHub App.";
    case "no_permission": return `GitHub has no permission in ${reason.repository}.`;
    case "no_workflow": return `${reason.repository} has no Ployz build workflow.`;
    case "multi_platform": return `GitHub builds one platform, and this image needs ${reason.platforms.join(" and ")}.`;
    case "dispatch_failed": return `GitHub couldn't start the build (${reason.message}).`;
    case "ended_before_start": return "The GitHub run ended before the build started.";
    case "runner_stopped": return "GitHub couldn't finish this build: its runner stopped.";
    case "no_push": return "GitHub couldn't finish this build: it pushed no image.";
    case "out_of_time": return "GitHub couldn't finish this build in time.";
    case "install_failed": return `GitHub couldn't install ployz ${reason.version}.`;
    case "preferred_unavailable": return preferredServerUnavailableText(reason.name);
    case "not_started": return reason.builder === "github"
      ? `No GitHub runner started within ${reason.minutes} min.`
      : `No server started the build within ${reason.minutes} min.`;
  }
}

/** The ployz version a runner couldn't install, as it reports it. */
export const installFailedSchema = Schema.String.check(Schema.isPattern(/^[0-9A-Za-z.+-]{1,64}$/u));

/** Mirrors core's `BuildGrantId` (ployz-core `value.rs`): 64 lowercase hex, its key's public half. */
const buildGrantIdSchema = Schema.declare<BuildGrantId>(
  (value): value is BuildGrantId => typeof value === "string" && /^[0-9a-f]{64}$/u.test(value),
);


/**
 * An Image Build GitHub Actions took: its dispatched run and why GitHub took it, then the Build
 * Grant and Build Steps once it started. Present exactly while GitHub is the Builder; the run id and
 * check-in time, which the race between check-in and a skip is decided on, are columns.
 */
export const githubImageBuildSchema = Schema.Struct({
  runUrl: Schema.String,
  /** The repository the run is in, as `owner/name`. */
  fullName: Schema.String,
  /** The `job_workflow_ref` the run's OIDC token must carry: the build workflow on the default branch. */
  workflowRef: Schema.String,
  reason: Schema.Literals(CANDIDATE_REASONS),
  /** The Build Grant minted at check-in, and the fingerprint the runner was told to build. */
  grant: Schema.NullOr(Schema.Struct({ id: buildGrantIdSchema, fingerprint: Schema.String.check(Schema.isPattern(/^[0-9a-f]{64}$/u)) })),
  /**
   * The runner's Build Steps so far; `platforms` once it reported its end (empty: it failed), and
   * `installFailed`, the ployz version it couldn't install, when it failed before building.
   */
  report: Schema.NullOr(Schema.Struct({
    received: Schema.Number,
    collector: collectorCheckpointSchema,
    platforms: Schema.NullOr(Schema.Array(Schema.String)),
    installFailed: Schema.optionalKey(installFailedSchema),
  })),
});
export type GithubImageBuild = typeof githubImageBuildSchema.Type;

/**
 * Why GitHub moves on a started build that ended without an image: it failed for GitHub's reasons,
 * not the build's. Null when a Build Step failed, in any batch: that is final.
 */
export function githubSkipReason(report: GithubImageBuild["report"], timedOut: boolean): SkipReason | null {
  // Before any Build Step ran: its failed "Installing ployz" step is GitHub's, not the build's.
  if (report?.installFailed !== undefined) return { builder: "github", kind: "install_failed", version: report.installFailed };
  if (report?.collector.stepFailed) return null;
  if (timedOut) return { builder: "github", kind: "out_of_time" };
  return report?.platforms ? { builder: "github", kind: "no_push" } : { builder: "github", kind: "runner_stopped" };
}
