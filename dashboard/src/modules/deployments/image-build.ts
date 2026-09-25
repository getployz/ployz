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
  /** The Service's Preferred Server is gone (`name` null) or no longer accepts builds: the walk went back to Auto. */
  Schema.Struct({ builder: Schema.Literal("servers"), kind: Schema.Literal("preferred_unavailable"), machineId: rustMachineIdSchema, name: Schema.NullOr(Schema.String) }),
  /** It hadn't started the build within its "start within" limit. */
  Schema.Struct({ builder: Schema.Literals(["github", "servers"]), kind: Schema.Literal("not_started"), minutes: Schema.Number }),
]);
export type SkipReason = typeof skipReasonSchema.Type;

/**
 * A Preferred Server that can't build, whether Cloud found it when planning the walk (a skip) or the
 * Engine did when choosing (its reason): the same words either way. `name` is null once it left the Cluster.
 */
export const preferredServerUnavailableText = (name: string | null) =>
  name === null ? "Preferred server: no longer in the Cluster" : `Preferred server ${name}: offline or no longer builds`;

/** Why a Builder didn't take an Image Build, as the canvas, the build log and a failed build say it. */
export function skipReasonText(reason: SkipReason): string {
  const builder = reason.builder === "github" ? "GitHub" : "Your servers";
  switch (reason.kind) {
    case "not_connected": return `${builder}: the repository isn't connected through the GitHub App`;
    case "no_permission": return `${builder}: no permission in ${reason.repository}`;
    case "no_workflow": return `${builder}: no workflow in ${reason.repository}`;
    case "multi_platform": return `${builder}: needs ${reason.platforms.join("+")}`;
    case "dispatch_failed": return `${builder}: could not start the build (${reason.message})`;
    case "ended_before_start": return `${builder}: the run ended before it started`;
    case "preferred_unavailable": return preferredServerUnavailableText(reason.name);
    case "not_started": return reason.builder === "github"
      ? `${builder}: no runner in ${reason.minutes} min`
      : `${builder}: none started it in ${reason.minutes} min`;
  }
}

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
  /** The runner's Build Steps so far; `platforms` once it reported its end (empty: it failed). */
  report: Schema.NullOr(Schema.Struct({
    received: Schema.Number,
    collector: collectorCheckpointSchema,
    platforms: Schema.NullOr(Schema.Array(Schema.String)),
  })),
});
export type GithubImageBuild = typeof githubImageBuildSchema.Type;
