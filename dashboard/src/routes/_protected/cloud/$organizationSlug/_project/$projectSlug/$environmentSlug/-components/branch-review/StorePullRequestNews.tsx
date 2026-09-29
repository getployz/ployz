import { Suspense, useState } from "react";
import { useParams } from "@tanstack/react-router";
import { toast } from "sonner";
import type { EnvironmentRef, PullRequestRef, PullRequestView } from "@ployz/sdk";
import { ArrowUpIcon, CircleCheckIcon, GitPullRequestIcon, TriangleAlertIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import { plural } from "#/modules/branches/branch-plan";
import { movePicks, presentMoveRow } from "#/modules/config-store/store-branches";
import { atMergeQuery, destinationNews, goLiveWhen, pullRequestQuery, type DestinationNews } from "#/modules/config-store/store-pull-requests";
import { useCachedStoreView, useStoreView } from "#/modules/config-store/store-view.queries";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";
import { actionVariant, NewsRow } from "./BranchNews";
import { SaveButton, saveInfo, Sheet, useRowPicks } from "./SaveSheet";

/**
 * A PR Environment's pull request, once read: its title for the panel's header, and the view its news reads. It waits
 * on the Branch's own view for the pull request it names, so it isn't prefetched; the news shows once it arrives.
 */
export function useStorePullRequest(pullRequest: PullRequestRef | null) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const result = useCachedStoreView(params.organizationSlug, pullRequest ? pullRequestQuery(pullRequest) : null);
  return result?.ok ? result.value : null;
}

/**
 * A PR Environment's news over the Config Store, one line per Destination its merge lands in: changes to save for the
 * merge (Save), a standing Conditional Save (Undo), one the Branch or target branch moved past (Save again). Then the
 * pull request's check on GitHub. The first line leads unless `lead` is false.
 */
export function StorePullRequestNews({ store, view, lead }: { store: EnvironmentRef; view: PullRequestView; lead: boolean }) {
  const pr = view.pull_request;
  const news = pr ? destinationNews(view, store.environment ?? "") : [];
  return (
    <>
      {news.map((item, index) => <DestinationRow key={item.into} store={store} news={item} number={pr?.number ?? 0} lead={lead && index === 0} />)}
      {pr?.open ? (
        <NewsRow icon={view.passing ? <CircleCheckIcon className="text-success" /> : <TriangleAlertIcon className="text-warning" />}
          title={view.passing ? "Ready to merge on GitHub" : "Not ready to merge on GitHub"} detail={view.reason} />
      ) : null}
    </>
  );
}

function DestinationRow({ store, news, number, lead }: { store: EnvironmentRef; news: DestinationNews; number: number; lead: boolean }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const writer = useStoreWriter(params.organizationSlug);
  const [saving, setSaving] = useState(false);
  const into = { project: store.project, environment: news.into };
  const sheet = saving ? (
    <Suspense fallback={null}>
      <AtMergeSheet from={store} into={into} number={number} onClose={() => setSaving(false)} />
    </Suspense>
  ) : null;
  if (news.kind === "saved") {
    return (
      <NewsRow lead={lead} icon={<GitPullRequestIcon />} title={`Saved for ${news.into}`} detail={goLiveWhen(news.changes, number)}
        // Withdraws it: the refetched view shows the changes to save again, and the check moves.
        action={<Button size="sm" variant="outline" onClick={() => void writer.commit({ command: "move", from: store, into, when: "at_merge", picks: [] })}>Undo</Button>} />
    );
  }
  return (
    <>
      <NewsRow lead={lead} icon={news.kind === "stale" ? <TriangleAlertIcon className="text-warning" /> : <ArrowUpIcon />}
        title={news.kind === "stale" ? `Changed since saved for ${news.into}` : `${plural(news.changes, "change")} to save`}
        detail={news.kind === "stale" ? "Save again to go live with the merge" : `${news.into} · go live when #${number} merges`}
        action={<Button size="sm" variant={actionVariant(lead)} onClick={() => setSaving(true)}>
          {news.kind === "stale" ? "Save again" : `Save to ${news.into}`}
        </Button>} />
      {sheet}
    </>
  );
}

/**
 * Save for the merge: what the PR Environment puts in one Destination, picked row by row, each variable its way. Nothing
 * changes there yet: it lands when the pull request merges, with the push that carries the merge.
 */
function AtMergeSheet({ from, into, number, onClose }: { from: EnvironmentRef; into: EnvironmentRef; number: number; onClose: () => void }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const writer = useStoreWriter(params.organizationSlug);
  const view = useStoreView(params.organizationSlug, atMergeQuery(from, into));
  const rows = useRowPicks((view.ok ? view.value.rows : []).map((row) => ({
    row: { key: row.row, node: !row.row.includes("."), conflict: row.conflict, choice: row.choice ?? undefined },
    presented: presentMoveRow(row),
  })));
  const [pending, setPending] = useState(false);
  const destination = into.environment ?? "";

  async function save() {
    if (!view.ok) return;
    setPending(true);
    try {
      // Awaited: the sheet closes once the Store holds it, and GitHub's check follows.
      await writer.commit({
        command: "move", from, into, when: "at_merge", version: view.value.version,
        picks: movePicks(rows.picks.map(({ row, pick }) => ({ key: row.key, ticked: pick.ticked, choice: row.choice, option: pick.option, value: pick.value }))),
      }).isPersisted.promise;
      toast.success(`Saved for #${number}'s merge into ${destination}`);
      onClose();
    } catch {
      // The writer toasted the refusal; a stale review shows the fresh rows.
    } finally {
      setPending(false);
    }
  }

  return (
    <Sheet title={`${plural(rows.picks.length, "change")} for ${destination}`} entries={rows.picks} picks={rows} destination={destination}
      info={!view.ok ? view.refusal.message : saveInfo(rows, destination) ?? `Nothing changes in ${destination} until #${number} merges.`}
      actions={<SaveButton picks={rows} destination={destination} pending={pending} onClick={() => void save()} />}
      onClose={onClose} />
  );
}
