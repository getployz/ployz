import type { RuleDraft } from "@ployz/sdk";
import { Badge } from "#/components/ui/badge";

type Summary = RuleDraft["summary"];

/** What deploys under the rule with no prompt: each push to its branch, or each image pushed to its repository. */
export function ruleTrigger(summary: Summary): { every: string; each: string } {
  return summary.source.kind === "github"
    ? { every: `every push to ${summary.source.branch}`, each: `each push to ${summary.source.branch}` }
    : { every: `every image pushed to ${summary.source.repository}`, each: `each image pushed to ${summary.source.repository}` };
}

/** What the passkey approves, line by line, as the CLI prints it before it signs. */
export function RuleSummaryList({ summary }: { summary: Summary }) {
  return (
    <dl className="flex flex-col gap-3 text-sm">
      <div className="flex flex-col gap-1">
        <dt className="text-muted-foreground">Namespace</dt>
        <dd className="font-mono">{summary.namespace}</dd>
      </div>
      {summary.source.kind === "github" ? (
        <>
          <div className="flex flex-col gap-1">
            <dt className="text-muted-foreground">Repository</dt>
            <dd><span className="font-mono">{summary.source.repository}</span> on <span className="font-mono">{summary.source.branch}</span></dd>
          </div>
          <div className="flex flex-col gap-1">
            <dt className="text-muted-foreground">Workflow</dt>
            <dd><span className="font-mono">{summary.source.workflow}</span>, on push and on rebuilds you approve</dd>
          </div>
          <div className="flex flex-col gap-1">
            <dt className="text-muted-foreground">Builder</dt>
            <dd className="break-all">{summary.source.pinned ? "Ployz's pinned release" : <span className="font-mono">{summary.source.builder}</span>}</dd>
          </div>
        </>
      ) : (
        <div className="flex flex-col gap-1">
          <dt className="text-muted-foreground">Images</dt>
          <dd>Every image pushed to <span className="font-mono">{summary.source.repository}</span></dd>
        </div>
      )}
      <div className="flex flex-col gap-1">
        <dt className="text-muted-foreground">Servers</dt>
        <dd>{summary.servers} in the cluster</dd>
      </div>
      {summary.services.map((service) => (
        <div key={service.name} className="flex flex-col gap-1">
          <dt className="flex flex-wrap items-center gap-2">
            <span className="font-medium">{service.name}</span>
            {service.changes.length > 0 ? <Badge variant="warning">Changed: {service.changes.join(", ")}</Badge> : null}
          </dt>
          {service.builds === "" ? null : <dd className="text-muted-foreground">{service.builds}</dd>}
          {service.settings.map((line) => <dd key={line} className="font-mono text-xs break-all">{line}</dd>)}
        </div>
      ))}
    </dl>
  );
}
