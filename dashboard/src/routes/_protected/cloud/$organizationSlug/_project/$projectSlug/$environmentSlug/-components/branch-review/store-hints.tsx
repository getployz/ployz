import { useLoaderData, useParams } from "@tanstack/react-router";
import type { DiffView, FollowHint, JsonValue, PullRequestHint } from "@ployz/sdk";
import { Button } from "#/components/ui/button";
import { Item, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemTitle } from "#/components/ui/item";
import { nodeName, presentMoveRow } from "#/modules/config-store/store-branches";
import { hintNotes } from "#/modules/config-store/store-pull-requests";
import { useStoreWriter } from "#/modules/config-store/store-write";
import type { ChangeGroup } from "#/modules/config-store/store-deployments";
import type { ChangeOrigin } from "../canvas/EnvironmentChangesReview";
import { ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";

/**
 * Details' notes over the Config Store:
 * - where a change came from, when it arrived from another Environment by Follow or Sync, and Never sync on such a
 *   setting;
 * - under a change, a merged pull request's value ("From PR #142" once staged; "PR #142: {value} · Use" where this
 *   Environment's own edit stayed), and the Parent's deployed value where this Branch's own change stayed
 *   ("production has since set {value} · Use theirs").
 * Hints no change shows, such as a Parent's change discarded here, come after the changes.
 *
 * `path` is a row's, or the group's `discardPath` for the node itself. `parent` is this Branch's; null on a root.
 */
export function storeHintNotes(diff: DiffView, groups: readonly ChangeGroup[], neverSync: (path: string) => void, parent: string | null) {
  // The Store names a Volume's settings as a Sync does, without `volumes.`.
  const paths = new Set(groups.flatMap((group) => group.rows.map((row) => nodeName(row.path))));
  const notes = hintNotes(diff.hints, paths);
  const follows = hintNotes(diff.follow_hints, paths);
  const source = (path: string) => diff.incoming.find((change) => change.row === nodeName(path))?.from;
  const name = diff.environment.name;
  return {
    originFor: (_: ChangeGroup, path: string): ChangeOrigin | undefined => {
      const from = source(path);
      if (!from) return undefined;
      // ponytail: the Store doesn't say whether Follow or a Sync brought it, so a Sync from the Parent reads as its deploy.
      return from === parent
        ? { title: `From ${from}'s deploy`, description: `${name} is a Branch of ${from}, so what ${from} deploys arrives here too.` }
        : { title: `From ${from}`, description: `${from} synced these here.` };
    },
    noteFor: (_: ChangeGroup, path: string) => {
      const prs = notes.at(nodeName(path));
      const parents = follows.at(nodeName(path));
      if (!prs.length && !parents.length) return null;
      return (
        <span className="flex flex-col items-start">
          {prs.map((hint) => <HintNote key={`${hint.save}:${hint.row}`} hint={hint} />)}
          {parents.map((hint) => <FollowNote key={hint.row} hint={hint} version={diff.version} />)}
        </span>
      );
    },
    // A setting that arrived; a whole node can't be marked.
    neverSyncFor: (group: ChangeGroup, path: string) =>
      path !== group.discardPath && source(path) ? () => neverSync(path) : undefined,
    after: notes.rest.length || follows.rest.length ? (
      <>
        {notes.rest.length ? (
          <ItemGroup role="group" aria-label="From merged pull requests">
            {notes.rest.map((hint) => (
              <Item key={`${hint.save}:${hint.row}`} variant="outline" size="sm">
                <ItemContent className="min-w-0">
                  <ItemTitle>{presented(hint).node} · {presented(hint).label}</ItemTitle>
                  {hint.landed === "staged" ? <ItemDescription className="font-mono">{presented(hint).after || "—"}</ItemDescription> : null}
                </ItemContent>
                <ItemActions><HintNote hint={hint} /></ItemActions>
              </Item>
            ))}
          </ItemGroup>
        ) : null}
        {follows.rest.length ? (
          <ItemGroup role="group" aria-label={`Not staged from ${follows.rest[0]?.from}`}>
            {follows.rest.map((hint) => (
              <Item key={hint.row} variant="outline" size="sm">
                <ItemContent className="min-w-0">
                  <ItemTitle>{presented(hint).node} · {presented(hint).label}</ItemTitle>
                </ItemContent>
                <ItemActions><FollowNote hint={hint} version={diff.version} /></ItemActions>
              </Item>
            ))}
          </ItemGroup>
        ) : null}
      </>
    ) : null,
  };
}

/** "From PR #142", and while this Environment's own edit stands, the pull request's value and Use. */
function HintNote({ hint }: { hint: PullRequestHint }) {
  const take = useTake();
  if (hint.landed === "staged") return <span className="text-muted-foreground">From PR #{hint.pull_request}</span>;
  return (
    <span className="flex min-w-0 items-center gap-1 text-muted-foreground">
      PR #{hint.pull_request}: <span className="truncate font-mono text-foreground">{presented(hint).after || "—"}</span>
      <Button variant="link" size="xs" aria-label={`Use PR #${hint.pull_request}'s ${hint.row}`}
        onClick={() => take(hint.save, hint.row)}>
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
      <Button variant="link" size="xs" aria-label={`Use theirs: ${hint.from}'s ${hint.row}`} onClick={() => take(hint.from, hint.row, version)}>
        Use theirs
      </Button>
    </span>
  );
}

/**
 * Stages a hint's value over this Environment's own, from a Conditional Save or the Parent; the refetched review
 * shows it, a refusal toasts.
 */
function useTake() {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { store } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const writer = useStoreWriter(params.organizationSlug);
  return (from: string, row: string, version?: string) =>
    void writer.commit({ command: "move", move: "take", from, into: store, rows: [row], version });
}

/** A hint in a Sync's words: its node and setting, and the value it offers (a secret stays hidden). */
const presented = (hint: { row: string; value: JsonValue }) => presentMoveRow({ row: hint.row, conflict: false, from: hint.value, into: null });
