import type { DeploymentView, GitSource } from "@ployz/sdk";
import { buildInterrupted, buildNodeId, heldBack, outcomeReason, waitingServices } from "#/modules/config-store/store-deployments";
import { cloudLink } from "#/modules/mail/cloud-link";
import type { MailContent, MailFact } from "#/modules/mail/layout";

export type DeployFailedMail = {
  readonly origin: string;
  readonly organization: { readonly name: string; readonly slug: string };
  readonly deployment: DeploymentView;
  readonly sources: readonly GitSource[];
};

export function deployFailedMail({ origin, organization, deployment, sources }: DeployFailedMail): Omit<MailContent, "unsubscribe"> {
  const { project, name: environment } = deployment.environment;
  const place = `${project} / ${environment}`;
  const byRuntime = new Map(Object.entries(deployment.runtime_names).map(([name, runtime]) => [runtime, name]));
  const serviceOf = (source: GitSource) => byRuntime.get(source.service) ?? source.service;
  // An interrupted build never said it failed: its Deployment ended first.
  const failed = deployment.builds.filter((build) => build.status === "failed" && !buildInterrupted(build));
  const failedBuild = failed[0];
  const serviceNodes = deployment.nodes.filter((node) => node.type === "service");
  const failedServices = [...new Set([
    ...serviceNodes.filter((node) => node.outcome === "failed").map((node) => node.name),
    ...failed.map((build) => build.service),
  ])];
  const services = failedServices.length === 0 && serviceNodes.length === 1 ? serviceNodes.map((node) => node.name) : failedServices;
  const named = services.length === 1 ? services[0] : services.length > 1 ? `${services.length} services` : undefined;
  const failure = outcomeReason(deployment.outcome) ?? failedBuild?.message ?? (deployment.status === "unknown"
    ? "Ployz lost track of it partway through, so some of it may be live."
    : "It stopped before it said why.");
  const source = sources.find((found) => serviceOf(found) === failedBuild?.service) ?? (sources.length === 1 ? sources[0] : undefined);
  const commit = source?.commit?.slice(0, 7);
  const job = failedBuild ? `job=${encodeURIComponent(buildNodeId(failedBuild.service))}` : undefined;
  const url = cloudLink(origin, [organization.slug, project, environment, "deployments", deployment.id], job);
  const facts: MailFact[] = [];
  if (source) facts.push({ label: "Repository", value: source.repository, mono: true });
  if (source?.branch) facts.push({ label: "Branch", value: source.branch, mono: true });
  if (commit) facts.push({ label: "Commit", value: commit, mono: true });
  if (services.length > 1) facts.push({ label: "Services", value: services.join(", ") });
  facts.push({ label: "Environment", value: place }, { label: "Error", value: failure });
  return {
    subject: named ? `${named} failed to deploy to ${place}` : `A deploy to ${place} failed`,
    preview: commit && source?.branch ? `${commit} on ${source.branch} didn't deploy.` : "The logs show what went wrong.",
    title: named ? `${named} failed to deploy to ${environment}` : `A deploy to ${environment} failed`,
    paragraphs: [source?.branch ? `GitHub started this deploy from ${source.branch}. The logs show what went wrong.` : "GitHub started this deploy. The logs show what went wrong."],
    action: { label: "View logs", url },
    facts,
    reason: `You're getting this as a member of ${organization.name} on Ployz.`,
  };
}

/**
 * The mail for a Deploy GitHub started that left Services waiting on something outside their config, whole or in some
 * regions: nobody watched it end, and what waits still runs what it ran before.
 */
export function deployWaitingMail({ origin, organization, deployment, sources }: DeployFailedMail): Omit<MailContent, "unsubscribe"> {
  const { project, name: environment } = deployment.environment;
  const place = `${project} / ${environment}`;
  const waiting = waitingServices(deployment);
  const [only] = waiting;
  const held = Object.values(only?.regions ?? {}).reduce((sum, region) => sum + (region?.waiting ?? 0) + (region?.outdated ?? 0), 0);
  const named = waiting.length !== 1 ? `${waiting.length} services are`
    : held === 0 ? `${only?.service} is`
    : held === 1 ? `1 replica of ${only?.service} is` : `${held} replicas of ${only?.service} are`;
  const source = sources.length === 1 ? sources[0] : undefined;
  const commit = source?.commit?.slice(0, 7);
  const facts: MailFact[] = [];
  if (source) facts.push({ label: "Repository", value: source.repository, mono: true });
  if (source?.branch) facts.push({ label: "Branch", value: source.branch, mono: true });
  if (commit) facts.push({ label: "Commit", value: commit, mono: true });
  facts.push({ label: "Environment", value: place }, ...waiting.map(({ service, reason, regions }) =>
    ({ label: service, value: regions && Object.keys(regions).length > 0 ? `${heldBack(regions)}, ${reason}` : reason })));
  const rest = deployment.all_waiting ? "Nothing was deployed." : "The rest of the deploy went through.";
  return {
    subject: `${named} waiting to deploy to ${place}`,
    preview: rest,
    title: `${named} waiting to deploy to ${environment}`,
    paragraphs: [`${source?.branch ? `GitHub started this deploy from ${source.branch}.` : "GitHub started this deploy."} ${rest} Deploy again once the cause is fixed.`],
    action: {
      label: "View deployment",
      url: cloudLink(origin, [organization.slug, project, environment, "deployments", deployment.id]),
    },
    facts,
    reason: `You're getting this as a member of ${organization.name} on Ployz.`,
  };
}
