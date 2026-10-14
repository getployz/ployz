import type { DeploymentView, GitSource } from "@ployz/sdk";
import { outcomeReason } from "#/modules/config-store/store-deployments";
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
  const failedBuild = deployment.builds.find((build) => build.status === "failed");
  const serviceNodes = deployment.nodes.filter((node) => node.type === "service");
  const failedServices = [...new Set([
    ...serviceNodes.filter((node) => node.outcome === "failed").map((node) => node.name),
    ...deployment.builds.filter((build) => build.status === "failed").map((build) => build.service),
  ])];
  const services = failedServices.length === 0 && serviceNodes.length === 1 ? serviceNodes.map((node) => node.name) : failedServices;
  const named = services.length === 1 ? services[0] : services.length > 1 ? `${services.length} services` : undefined;
  const failure = outcomeReason(deployment.outcome) ?? failedBuild?.message ?? (deployment.status === "unknown"
    ? "Ployz lost track of it partway through, so some of it may be live."
    : "It stopped before it said why.");
  const source = sources.find((found) => serviceOf(found) === failedBuild?.service) ?? (sources.length === 1 ? sources[0] : undefined);
  const commit = source?.commit?.slice(0, 7);
  const segment = (value: string) => encodeURIComponent(value);
  const failedNode = deployment.nodes.find((node) => node.type === "service" && node.name === failedBuild?.service);
  const logs = failedBuild ? `build${failedNode ? `&service=${segment(failedNode.id)}` : ""}` : "deploy";
  const url = `${origin}/cloud/${segment(organization.slug)}/${segment(project)}/${segment(environment)}/deployments/${segment(deployment.id)}?logs=${logs}`;
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
