import { useParams } from "@tanstack/react-router";
import { GitPullRequestIcon } from "lucide-react";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { presentRow, rowPath, type PresentedRow } from "#/modules/branches/branch-review";
import { useLineageNames } from "#/modules/branches/use-lineage-names";
import { useEnvironmentDocument } from "#/modules/environment-design/environment-document.collection";
import type { CanvasEnvironmentChangeGroup } from "#/modules/environment-design/canvas-environment-change-state";
import { useLandedSaves } from "#/modules/pr-environments/conditional-save.collection";
import { useTakePullRequestValue } from "#/modules/pr-environments/conditional-save-commands";
import type { ConditionalSaveRow, HeldRow } from "#/modules/pr-environments/tables";
import { ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";

type Landed = { save: ConditionalSaveRow; held: HeldRow; presented: PresentedRow };

/**
 * A merged pull request's settings this Environment had changed too, beside its changes to deploy: a change it staged is
 * tagged "PR #142"; beside an edit of its own, "PR #142: {value} · Use". `noteFor` finds a change's note; `rest` lists
 * the ones no change to deploy shows, such as a variable.
 */
export function useLandedNotes(environmentId: string, groups: CanvasEnvironmentChangeGroup[]) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const landed = useLandedSaves(params.organizationSlug, environmentId);
  const document = useEnvironmentDocument(params.organizationSlug, environmentId);
  const lineageName = useLineageNames(params.organizationSlug);
  const take = useTakePullRequestValue(params.organizationSlug, environmentId);
  const entries = landed.flatMap((save) => save.rows.flatMap((held): Landed[] => held.landed
    ? [{ save, held, presented: presentRow(held.row, (lineage) => lineageName(lineage, environmentId)) }] : []));
  const keyOf = ({ presented, held }: Landed) =>
    `${document?.intent.services.find((node) => node.lineageId === presented.lineageId)?.id}:${rowPath(held.row)}`;
  const shown = new Set(groups.flatMap((group) => group.rows.map((row) => `${group.nodeId}:${row.path}`)));
  const note = ({ save, held, presented }: Landed) => held.landed === "staged" ? (
    <Badge variant="outline"><GitPullRequestIcon data-icon="inline-start" />PR #{save.prNumber}</Badge>
  ) : (
    <span className="flex min-w-0 items-center gap-1 text-xs text-muted-foreground">
      <GitPullRequestIcon className="size-3 shrink-0" />
      <span className="truncate">PR #{save.prNumber}: <span className="font-mono text-foreground">{presented.after || "—"}</span></span>
      <Button variant="link" size="xs" disabled={take.isPending} aria-label={`Use PR #${save.prNumber}'s ${presented.label}`}
        onClick={() => take.mutate({ conditionalSaveId: save.id, key: held.row.key })}>Use</Button>
    </span>
  );
  const rest = entries.filter((entry) => !shown.has(keyOf(entry)));
  return {
    noteFor: (group: CanvasEnvironmentChangeGroup, path: string) => {
      const entry = entries.find((candidate) => keyOf(candidate) === `${group.nodeId}:${path}`);
      return entry ? note(entry) : null;
    },
    rest: rest.length ? (
      <section aria-label="From merged pull requests" className="flex flex-col gap-2 rounded-lg border px-3 py-2 text-sm">
        {rest.map((entry) => (
          <div key={`${entry.save.id}:${entry.held.row.key}`} className="flex flex-wrap items-center gap-x-2 gap-y-1">
            <span className="font-medium">{entry.presented.node} · {entry.presented.label}</span>
            {entry.held.landed === "staged" ? <span className="font-mono text-xs">{entry.presented.after || "—"}</span> : null}
            {note(entry)}
          </div>
        ))}
      </section>
    ) : null,
  };
}
