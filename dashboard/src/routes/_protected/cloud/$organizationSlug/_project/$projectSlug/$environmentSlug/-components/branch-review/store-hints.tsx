import { useLoaderData, useParams } from "@tanstack/react-router";
import type { DiffView, PullRequestHint } from "@ployz/sdk";
import { GitPullRequestIcon } from "lucide-react";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Item, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemTitle } from "#/components/ui/item";
import { presentMoveRow } from "#/modules/config-store/store-branches";
import { hintNotes } from "#/modules/config-store/store-pull-requests";
import { useStoreWriter } from "#/modules/config-store/store-write";
import type { ChangeGroup } from "#/modules/config-store/store-deployments";
import { ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";

/**
 * Details' notes over the Config Store: a merged pull request's value landed here beside the change to deploy at its
 * path ("PR #142" once staged; "PR #142: {value} · Use" where this Environment's own edit stayed), and the ones no change
 * shows after the changes.
 */
export function storeHintNotes(diff: DiffView, groups: readonly ChangeGroup[]) {
  const notes = hintNotes(diff.hints, new Set(groups.flatMap((group) => group.rows.map((row) => row.path))));
  return {
    noteFor: (_: ChangeGroup, path: string) => {
      const at = notes.at(path);
      return at.length ? <>{at.map((hint) => <HintNote key={`${hint.save}:${hint.row}`} hint={hint} />)}</> : null;
    },
    after: notes.rest.length ? (
      <ItemGroup aria-label="From merged pull requests">
        {notes.rest.map((hint) => (
          <Item key={`${hint.save}:${hint.row}`} variant="outline" size="sm">
            <ItemContent className="min-w-0">
              <ItemTitle>{presented(hint).node} · {presented(hint).label}</ItemTitle>
              {hint.landed === "staged" ? <ItemDescription className="ph-no-capture font-mono">{presented(hint).after || "—"}</ItemDescription> : null}
            </ItemContent>
            <ItemActions><HintNote hint={hint} /></ItemActions>
          </Item>
        ))}
      </ItemGroup>
    ) : null,
  };
}

/** The "PR #142" tag, and while this Environment's own edit stands, the pull request's value and Use. */
function HintNote({ hint }: { hint: PullRequestHint }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { store } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const writer = useStoreWriter(params.organizationSlug);
  const tag = <Badge variant="outline"><GitPullRequestIcon data-icon="inline-start" />PR #{hint.pull_request}</Badge>;
  if (hint.landed === "staged") return tag;
  return (
    <span className="flex min-w-0 items-center gap-1 text-muted-foreground">
      {tag}<span className="ph-no-capture truncate font-mono text-foreground">{presented(hint).after || "—"}</span>
      <Button variant="link" size="xs" aria-label={`Use PR #${hint.pull_request}'s ${hint.row}`}
        // Stages the pull request's value over this Environment's own; the refetched review shows it, a refusal toasts.
        onClick={() => void writer.commit({ command: "move", move: "take", from: hint.save, into: store, rows: [hint.row] })}>
        Use
      </Button>
    </span>
  );
}

/** A hint in a Save sheet's words: its node and setting, and the pull request's value (a secret stays hidden). */
const presented = (hint: PullRequestHint) => presentMoveRow({ row: hint.row, conflict: false, from: hint.value, into: null });
