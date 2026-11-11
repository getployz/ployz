/**
 * The deploys the dashboard starts outside its Deploy button, approved in this browser on Servers that enforce Device
 * approval: a redeploy, which is also how a rollback and a retry of a failed Deployment run, and a Deploy of another
 * Environment, a new Branch's first or a shut-down preview's. Each reads what it ships, shows the person every term this
 * browser will sign, read back from the bodies it drafted, and signs exactly those bodies once they approve.
 *
 * Cloud relays the plan, the commits and the rule. What it relays may show, but nothing here checks it: only Servers do.
 *
 *   redeploy of images   plan of the Deployment ─▶ draft DeployApproval ─▶ review ─▶ sign ─▶ Retry { approval }
 *   redeploy of builds   rule ─▶ draft CommitApprovals ─▶ review ─▶ sign ─▶ Retry { approvals, awaits_approval }
 *                        ─▶ builds end ─▶ the same commits again, new times, no ask ─▶ ApproveDeployment
 *   other Environment    plan ─▶ draft DeployApproval ─▶ review ─▶ sign ─▶ Deploy { approval, version }
 */
import type { ConfigWritten, DeploymentView, EnvironmentRef, PlanView, Retry, RuleView, Signed } from "@ployz/sdk";
import { Option, Schema } from "effect";
import { deployApprovalScope, type DeployTarget, ensureApproval, type Review } from "#/modules/approval-session/approval-prompt";
import {
  type ApprovalScope, COMMIT_CAPABILITIES, type CommitTerms, draftCommits, draftDeploy, type Drafted, SessionRefused, signDrafted,
} from "#/modules/approval-session/approval-session";

/** The Store reads an approval takes, fresh: a plan, of a Deployment's redeploy when named, a rule, a Deployment, a Namespace. */
export type Reads = {
  readonly plan: (environment: EnvironmentRef, deployment?: string) => Promise<PlanView>;
  readonly namespace: (environment: EnvironmentRef) => Promise<string>;
  readonly rule: (environment: EnvironmentRef) => Promise<RuleView>;
  readonly deployment: (id: string) => Promise<DeploymentView>;
};

/** Every Server, and those running a Namespace now, as this tab last saw them: where a deploy may place. */
export type Servers = { readonly machines: DeployTarget["machines"]; readonly running: (namespace: string) => readonly string[] };

/** What an approval reads and asks: the Store, the Servers, and the person, who answers each review. */
export type Approving = { readonly reads: Reads; readonly servers: Servers; readonly review: (review: Review) => Promise<boolean> };

const SIGNS_FOR = "Your servers run only what you approve here, and check it themselves.";

const enforcing = (servers: Servers) => servers.machines.some((machine) => machine.deviceEnforcement === true);

/** Shows `drafted` and signs exactly its bodies once the person approves. Null when they cancel: nothing is signed. */
async function reviewed(approving: Approving, drafted: Drafted, title: string, note = SIGNS_FOR, action = "Approve") {
  if (!(await approving.review({ title, lines: drafted.lines, note, action }))) return null;
  return signDrafted(drafted);
}

const scopeOf = (environment: { readonly id: string; readonly name: string }, namespace: string) =>
  ({ environmentId: environment.id, environmentName: environment.name, namespace });

/**
 * A Deploy of `environment`, as Deployment `id`, approved in this browser: its plan read now, every term shown, then
 * signed. The version it read goes with it, so the Store refuses it once the Environment changes. One that runs nowhere
 * yet, a new Branch or a shut-down preview, names the Cluster `near` runs on, its Parent. Undefined when no Server
 * enforces, so the Deploy goes as before; null when the person cancels.
 */
export async function approveEnvironmentDeploy(approving: Approving, environment: EnvironmentRef, id: string, near: EnvironmentRef) {
  const { reads, servers } = approving;
  if (!enforcing(servers)) return undefined;
  const plan = await reads.plan(environment);
  const own = servers.running(plan.namespace);
  const environmentMachines = own.length > 0 ? own : servers.running(await reads.namespace(near));
  const scope = await deployApprovalScope({ environment: scopeOf(plan.environment, plan.namespace), environmentMachines, machines: servers.machines });
  if (scope === undefined || scope === null) return scope;
  const signed = await reviewed(approving, await draftDeploy(scope, id, plan), `Deploy ${plan.environment.name}?`);
  const [approval] = signed ?? [];
  return approval === undefined ? null : { approval, version: plan.version };
}

/** A redeploy's approvals, as the Store's Retry takes them, and what its builds wait on before it runs. */
export type RedeployApproved = {
  readonly fields: Pick<Retry, "approval" | "approvals" | "awaits_approval">;
  /** For a redeploy that builds: the terms the person approved, approved again once its builds end. */
  readonly again: Again | null;
};

export type Again = { readonly scope: ApprovalScope; readonly terms: CommitTerms };

/** The fields of an automatic deploy rule's body a commit approval echoes. */
const RuleBody = Schema.fromJsonString(Schema.Struct({ rule: Schema.String, generation: Schema.Number }));

/** The automatic deploy rule's ID and generation as Cloud relays them, which each Server compares with its own. */
function ruleTerms(view: RuleView) {
  if (view.rule === null || view.rule === undefined) {
    throw new SessionRefused(`${view.environment.name} has no automatic deploy rule. Approve one, then redeploy. Nothing deployed.`);
  }
  const parsed = Schema.decodeUnknownOption(RuleBody)(view.rule.body);
  if (Option.isNone(parsed)) throw new SessionRefused("This browser couldn't read the automatic deploy rule. Nothing deployed.");
  return parsed.value;
}

/** Each Service `source` built and the commit it built from. Refused for one built from an upload, which no approval can name. */
function commitsOf(source: DeploymentView) {
  return source.builds.map((build) => {
    if (build.commit === null) {
      throw new SessionRefused(`${build.service} builds from an upload. Servers that enforce device approval run only what GitHub built from a commit. Nothing deployed.`);
    }
    return { service: build.service, commit: build.commit };
  });
}

/**
 * A redeploy of `source` as Deployment `id`, approved in this browser. One that built from GitHub approves each Service's
 * commit under the Environment's automatic deploy rule, as `ployz deployment redeploy` does, and waits for its builds to
 * approve them again. Any other approves the specs its runner will lower, read as a plan of `source`. Undefined when no
 * Server enforces, so the redeploy goes as before; null when the person cancels.
 */
export async function approveRedeploy(approving: Approving, source: DeploymentView, id: string): Promise<RedeployApproved | null | undefined> {
  const { reads, servers } = approving;
  if (!enforcing(servers)) return undefined;
  const environment = { project: source.environment.project, environment: source.environment.name };
  const target = { environment: scopeOf(source.environment, source.namespace), environmentMachines: servers.running(source.namespace), machines: servers.machines };
  if (source.builds.length > 0) {
    const commits = commitsOf(source);
    const scope = await deployApprovalScope(target, COMMIT_CAPABILITIES);
    if (scope === undefined || scope === null) return scope;
    const terms = { ...ruleTerms(await reads.rule(environment)), commits };
    const approvals = await reviewed(approving, await draftCommits(scope, terms), `Approve the commits of this redeploy?`,
      "Your servers run only what GitHub builds from these commits under your rule. Once each image is built or reused, this browser approves the same commits again without asking, so keep this tab open until it deploys.");
    return approvals === null ? null : { fields: { approvals, awaits_approval: true }, again: { scope, terms } };
  }
  const scope = await deployApprovalScope(target);
  if (scope === undefined || scope === null) return scope;
  const plan = await reads.plan(environment, source.id);
  const signed = await reviewed(approving, await draftDeploy(scope, id, plan), `Redeploy ${source.environment.name}?`);
  const [approval] = signed ?? [];
  return approval === undefined ? null : { fields: { approval }, again: null };
}

/** How long a waiting redeploy is read again while its builds run. */
export const BUILD_LOOK_MS = 3_000;

export type Follow = {
  readonly read: () => Promise<DeploymentView>;
  readonly approve: (approvals: Signed[]) => Promise<ConfigWritten>;
  readonly cancel: () => Promise<ConfigWritten>;
  readonly wait: () => Promise<void>;
};

/**
 * Waits for a redeploy's builds, then approves the same commits again with new times, without asking, and hands them to
 * the Store, which lets it run: an approval lasts 15 minutes, and a build may take longer. Cancels the redeploy on any
 * failure, so nothing waits for an approval that won't come.
 */
export async function approveAgain(follow: Follow, again: Again): Promise<void> {
  try {
    for (;;) {
      const view = await follow.read();
      if (!view.in_flight) throw new SessionRefused("The redeploy ended before this browser approved it. Nothing deployed.");
      for (const build of view.builds) {
        const at = build.commit === null ? build.service : `${build.service} at ${build.commit.slice(0, 12)}`;
        if (build.github_skipped !== null) throw new SessionRefused(`GitHub didn't build ${at}: ${build.github_skipped}. Nothing deployed.`);
        if (build.status === "failed") throw new SessionRefused(`GitHub didn't build ${at}: ${build.message ?? "its build failed"}. Nothing deployed.`);
      }
      if (view.builds.every((build) => build.status === "built" || build.status === "reused")) break;
      await follow.wait();
    }
    if (!(await ensureApproval(again.scope, "deploy"))) throw new SessionRefused("Cancelled. Nothing deployed.");
    await follow.approve(await signDrafted(await draftCommits(again.scope, again.terms)));
  } catch (error) {
    // A failed cancel leaves the claim to fail it: it won't run unapproved.
    await follow.cancel().catch(() => undefined);
    throw error;
  }
}
