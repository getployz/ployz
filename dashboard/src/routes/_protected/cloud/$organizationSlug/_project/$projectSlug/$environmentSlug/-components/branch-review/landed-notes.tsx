import { useParams } from "@tanstack/react-router";
import { GitPullRequestIcon } from "lucide-react";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Item, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemTitle } from "#/components/ui/item";
import { presentRow, rowPath, type PresentedRow } from "#/modules/branches/branch-review";
import { useLineageNames } from "#/modules/branches/use-lineage-names";
import { useEnvironmentDocument } from "#/modules/environment-design/environment-document.collection";
import type { CanvasEnvironmentChangeGroup } from "#/modules/environment-design/canvas-environment-change-state";
import { useLandedSaves } from "#/modules/pr-environments/conditional-save.collection";
import { useTakePullRequestValue } from "#/modules/pr-environments/conditional-save-commands";
import type { ConditionalSaveRow, SavedRow } from "#/modules/pr-environments/tables";
import { ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";

/** A merged pull request's setting this Environment had changed too, on its node (`nodeId`) at `path`. */
export type Landed = { save: ConditionalSaveRow; saved: SavedRow; presented: PresentedRow; nodeId: string | undefined; path: string };

/**
 * A merged pull request's settings this Environment had changed too, beside its changes to deploy: a change it staged is
 * tagged "PR #142"; beside an edit of its own, "PR #142: {value} · Use". `rest` are the ones no change to deploy shows,
 * such as a variable.
 */
export function useLandedNotes(environmentId: string, groups: CanvasEnvironmentChangeGroup[]) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const landed = useLandedSaves(params.organizationSlug, environmentId);
  const document = useEnvironmentDocument(params.organizationSlug, environmentId);
  const lineageName = useLineageNames(params.organizationSlug);
  const notes = landed.flatMap((save) => save.rows.flatMap((saved): Landed[] => {
    if (!saved.landed) return [];
    const presented = presentRow(saved.row, (lineage) => lineageName(lineage, environmentId));
    const nodeId = document?.intent.services.find((node) => node.lineageId === presented.lineageId)?.id;
    return [{ save, saved, presented, nodeId, path: rowPath(saved.row) }];
  }));
  const shown = (note: Landed) => groups.some((group) => group.nodeId === note.nodeId && group.rows.some((row) => row.path === note.path));
  return { notes, rest: notes.filter((note) => !shown(note)) };
}

/** Its note: the "PR #142" tag, or the pull request's value and Use. */
export function LandedNote({ note: { save, saved, presented } }: { note: Landed }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const take = useTakePullRequestValue(params.organizationSlug, save.destinationEnvironmentId);
  const tag = <Badge variant="outline"><GitPullRequestIcon data-icon="inline-start" />PR #{save.prNumber}</Badge>;
  if (saved.landed === "staged") return tag;
  return (
    <span className="flex min-w-0 items-center gap-1 text-muted-foreground">
      {tag}<span className="truncate font-mono text-foreground">{presented.after || "—"}</span>
      <Button variant="link" size="xs" disabled={take.isPending} aria-label={`Use PR #${save.prNumber}'s ${presented.label}`}
        onClick={() => take.mutate({ conditionalSaveId: save.id, key: saved.row.key })}>Use</Button>
    </span>
  );
}

/** The ones no change to deploy shows, listed after the changes. */
export function LandedRest({ notes }: { notes: Landed[] }) {
  if (!notes.length) return null;
  return (
    <ItemGroup aria-label="From merged pull requests">
      {notes.map((note) => (
        <Item key={`${note.save.id}:${note.saved.row.key}`} variant="outline" size="sm">
          <ItemContent className="min-w-0">
            <ItemTitle>{note.presented.node} · {note.presented.label}</ItemTitle>
            {note.saved.landed === "staged" ? <ItemDescription className="font-mono">{note.presented.after || "—"}</ItemDescription> : null}
          </ItemContent>
          <ItemActions><LandedNote note={note} /></ItemActions>
        </Item>
      ))}
    </ItemGroup>
  );
}
