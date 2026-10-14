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
  const services = [...new Set(sources.map((source) => source.service))];
  const named = services.length === 1 ? services[0] : services.length > 1 ? `${services.length} services` : undefined;
  const failedBuild = deployment.builds.find((build) => build.status === "failed");
  const failure = outcomeReason(deployment.outcome) ?? failedBuild?.message ?? (deployment.status === "unknown"
    ? "Cloud lost track of it partway, so some of it may have applied."
    : "It failed before it reported why.");
  const source = sources.find((found) => found.service === failedBuild?.service) ?? sources[0];
  const commit = source?.commit?.slice(0, 7);
  const segment = (value: string) => encodeURIComponent(value);
  const failedNode = deployment.nodes.find((node) => node.type === "service" && node.name === failedBuild?.service);
  const logs = failedBuild ? `build${failedNode ? `&service=${segment(failedNode.id)}` : ""}` : "deploy";
  const url = `${origin}/cloud/${segment(organization.slug)}/${segment(project)}/${segment(environment)}/deployments/${segment(deployment.id)}?logs=${logs}`;
  const facts: MailFact[] = [];
  if (source) facts.push({ label: "Repository", value: source.repository, mono: true });
  if (source?.branch) facts.push({ label: "Branch", value: source.branch, mono: true });
  if (commit) facts.push({ label: "Commit", value: commit, mono: true });
  if (services.length > 0) facts.push({ label: services.length === 1 ? "Service" : "Services", value: services.join(", ") });
  facts.push({ label: "Environment", value: place }, { label: "Failed with", value: failure });
  return {
    subject: named ? `Deploy failed: ${named} in ${place}` : `Deploy failed in ${place}`,
    preview: commit && source?.branch ? `${commit} on ${source.branch} failed. The logs show why.` : "The logs show why.",
    title: named ? `${named} failed to deploy to ${environment}` : `A deploy to ${environment} failed`,
    paragraphs: [source?.branch ? `GitHub started this deploy from ${source.branch}, and it failed. The logs show why.` : "GitHub started this deploy, and it failed. The logs show why."],
    action: { label: "View logs", url },
    facts,
    reason: `You're getting this because you're a member of ${organization.name} on Ployz.`,
  };
}
