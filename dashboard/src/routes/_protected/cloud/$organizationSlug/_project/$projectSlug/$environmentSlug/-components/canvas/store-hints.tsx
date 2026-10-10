import { useLoaderData, useParams } from "@tanstack/react-router";
import type { DiffView, FollowHint, RowId } from "@ployz/sdk";
import { Button } from "#/components/ui/button";
import { Item, ItemActions, ItemContent, ItemGroup, ItemTitle } from "#/components/ui/item";
import { presentRow } from "#/modules/config-store/store-branches";
import { hintNotes } from "#/modules/config-store/store-pull-requests";
import { useStoreWriter } from "#/modules/config-store/store-write";
import type { ChangeGroup, ChangeRow } from "#/modules/config-store/store-deployments";
import type { ChangeOrigin } from "./EnvironmentChangesReview";
import { ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";

/**
 * Details' notes over the Config Store:
 * - where a change came from, when Follow brought it from the Parent's deploy, and Never sync on such a setting;
 * - under a change, the Parent's deployed value where this Branch's own change stayed ("production has since set
 *   {value} · Use theirs").
 * Hints no change shows, such as a Parent's change discarded here, come after the changes.
 *
 * Each joins a change to them by the Sync row it falls in: a setting's, or without one, its node's.
 */
export function storeHintNotes(diff: DiffView, groups: readonly ChangeGroup[], neverSync: (path: string, row: RowId) => void) {
  const rows = new Set(groups.flatMap((group) => group.rows.flatMap((row) => row.row ?? [])));
  const follows = hintNotes(diff.follow_hints, rows);
  const incoming = (row: RowId | null) => diff.incoming.find((change) => change.row === row);
  const name = diff.environment.name;
  // Hints no change shows.
  const rest = follows.rest.map((hint) => ({ key: `${hint.from}:${hint.row}`, hint, note: <FollowNote hint={hint} version={diff.version} /> }));
  return {
    originFor: (row: RowId): ChangeOrigin | undefined => {
      const from = incoming(row)?.from;
      return from
        ? { title: `From ${from}'s deploy`, description: `${name} is a Branch of ${from}, so what ${from} deploys arrives here too.` }
        : undefined;
    },
    noteFor: (row: ChangeRow) => {
      const parents = follows.at(row.row);
      if (!parents.length) return null;
      return (
        <span className="flex flex-col items-start">
          {parents.map((hint) => <FollowNote key={hint.row} hint={hint} version={diff.version} />)}
        </span>
      );
    },
    // A setting that arrived; a whole node can't be marked.
    neverSyncFor: (row: ChangeRow) => {
      const change = incoming(row.row);
      return change && change.name !== null ? () => neverSync(row.path, change.row) : undefined;
    },
    after: rest.length ? (
      <ItemGroup role="group" aria-label="Not among these changes">
        {rest.map(({ key, hint, note }) => {
          const shown = presentRow(hint);
          return (
            <Item key={key} variant="outline" size="sm">
              <ItemContent className="min-w-0">
                <ItemTitle>{shown.node} · {shown.label}</ItemTitle>
              </ItemContent>
              <ItemActions>{note}</ItemActions>
            </Item>
          );
        })}
      </ItemGroup>
    ) : null,
  };
}

/** The Parent's deployed value this Branch didn't stage, and Use theirs. */
function FollowNote({ hint, version }: { hint: FollowHint; version: string }) {
  const take = useTake();
  const shown = presentRow(hint);
  return (
    <span className="flex min-w-0 items-center gap-1 text-muted-foreground">
      {hint.from} has since set <span className="ph-no-capture truncate font-mono text-foreground">{shown.after || "—"}</span>
      <Button variant="link" size="xs" aria-label={`Use theirs: ${hint.from}'s ${shown.node} ${shown.label}`} onClick={() => take(hint.from, hint.row, version)}>
        Use theirs
      </Button>
    </span>
  );
}

/**
 * Stages a hint's value over this Environment's own, from the Parent, if Details is still as the
 * user sees it (`version`); the refetched review shows it, a refusal toasts.
 */
function useTake() {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { store } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const writer = useStoreWriter(params.organizationSlug);
  return (from: string, row: RowId, version: string) =>
    void writer.commit({ command: "take", from, into: store, rows: [row], version });
}
