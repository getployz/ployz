import { useLoaderData, useParams } from "@tanstack/react-router";
import type { DiffView, FollowHint, PullRequestHint, RowId } from "@ployz/sdk";
import { Button } from "#/components/ui/button";
import { Item, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemTitle } from "#/components/ui/item";
import { presentRow, rowPath } from "#/modules/config-store/store-branches";
import { hintNotes } from "#/modules/config-store/store-pull-requests";
import { useStoreWriter } from "#/modules/config-store/store-write";
import type { ChangeGroup } from "#/modules/config-store/store-deployments";
import type { ChangeOrigin } from "./EnvironmentChangesReview";
import { ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";

/**
 * Details' notes over the Config Store:
 * - where a change came from, when Follow brought it from the Parent's deploy, and Never sync on such a setting;
 * - under a change, a merged pull request's value ("From PR #142" once staged; "PR #142: {value} · Use" where this
 *   Environment's own edit stayed), and the Parent's deployed value where this Branch's own change stayed
 *   ("production has since set {value} · Use theirs").
 * Hints no change shows, such as a Parent's change discarded here, come after the changes.
 *
 * `path` is a row's, or the group's `discardPath` for the node itself: what the Store's rows line up with.
 */
export function storeHintNotes(diff: DiffView, groups: readonly ChangeGroup[], neverSync: (path: string, row: RowId) => void) {
  const paths = new Set(groups.flatMap((group) => group.rows.map((row) => row.path)));
  const notes = hintNotes(diff.hints, paths);
  const follows = hintNotes(diff.follow_hints, paths);
  const incoming = (path: string) => diff.incoming.find((change) => rowPath(change) === path);
  const name = diff.environment.name;
  // Hints no change shows, both kinds in one list.
  const rest = [
    ...notes.rest.map((hint) => ({
      key: `${hint.conditional_sync}:${hint.row}`, hint, note: <HintNote hint={hint} version={diff.version} />,
      staged: hint.landed === "staged",
    })),
    ...follows.rest.map((hint) => ({ key: `${hint.from}:${hint.row}`, hint, note: <FollowNote hint={hint} version={diff.version} />, staged: false })),
  ];
  return {
    originFor: (_: ChangeGroup, path: string): ChangeOrigin | undefined => {
      const from = incoming(path)?.from;
      return from
        ? { title: `From ${from}'s deploy`, description: `${name} is a Branch of ${from}, so what ${from} deploys arrives here too.` }
        : undefined;
    },
    noteFor: (_: ChangeGroup, path: string) => {
      const prs = notes.at(path);
      const parents = follows.at(path);
      if (!prs.length && !parents.length) return null;
      return (
        <span className="flex flex-col items-start">
          {prs.map((hint) => <HintNote key={`${hint.conditional_sync}:${hint.row}`} hint={hint} version={diff.version} />)}
          {parents.map((hint) => <FollowNote key={hint.row} hint={hint} version={diff.version} />)}
        </span>
      );
    },
    // A setting that arrived; a whole node can't be marked.
    neverSyncFor: (_: ChangeGroup, path: string) => {
      const change = incoming(path);
      return change && change.name !== null ? () => neverSync(path, change.row) : undefined;
    },
    after: rest.length ? (
      <ItemGroup role="group" aria-label="Not among these changes">
        {rest.map(({ key, hint, note, staged }) => (
          <Item key={key} variant="outline" size="sm">
            <ItemContent className="min-w-0">
              <ItemTitle>{presented(hint).node} · {presented(hint).label}</ItemTitle>
              {staged ? <ItemDescription className="font-mono">{presented(hint).after || "—"}</ItemDescription> : null}
            </ItemContent>
            <ItemActions>{note}</ItemActions>
          </Item>
        ))}
      </ItemGroup>
    ) : null,
  };
}

/** "From PR #142", and while this Environment's own edit stands, the pull request's value and Use. */
function HintNote({ hint, version }: { hint: PullRequestHint; version: string }) {
  const take = useTake();
  if (hint.landed === "staged") return <span className="text-muted-foreground">From PR #{hint.pull_request}</span>;
  return (
    <span className="flex min-w-0 items-center gap-1 text-muted-foreground">
      PR #{hint.pull_request}: <span className="truncate font-mono text-foreground">{presented(hint).after || "—"}</span>
      <Button variant="link" size="xs" aria-label={`Use PR #${hint.pull_request}'s ${rowPath(hint)}`}
        onClick={() => take(hint.conditional_sync, hint.row, version)}>
        Use
      </Button>
    </span>
  );
}

/** The Parent's deployed value this Branch didn't stage, and Use theirs. */
function FollowNote({ hint, version }: { hint: FollowHint; version: string }) {
  const take = useTake();
  return (
    <span className="flex min-w-0 items-center gap-1 text-muted-foreground">
      {hint.from} has since set <span className="truncate font-mono text-foreground">{presented(hint).after || "—"}</span>
      <Button variant="link" size="xs" aria-label={`Use theirs: ${hint.from}'s ${rowPath(hint)}`} onClick={() => take(hint.from, hint.row, version)}>
        Use theirs
      </Button>
    </span>
  );
}

/**
 * Stages a hint's value over this Environment's own, from a Conditional Sync or the Parent, if Details is still as the
 * user sees it (`version`); the refetched review shows it, a refusal toasts.
 */
function useTake() {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { store } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const writer = useStoreWriter(params.organizationSlug);
  return (from: string, row: RowId, version: string) =>
    void writer.commit({ command: "take", from, into: store, rows: [row], version });
}

/** A hint in a Sync's words: its node and setting, and the value it offers (a secret stays hidden). */
const presented = (hint: PullRequestHint | FollowHint) => presentRow(hint);
