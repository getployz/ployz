import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { ArchiveRestoreIcon, FileQuestionIcon, HardDriveIcon, Trash2Icon } from "lucide-react";
import type { TrashEntry, TrashList } from "@ployz/sdk";
import { DeletionDialog } from "#/components/deletion-dialog";
import { SettingsEmpty, SettingsList, SettingsListItem } from "#/components/settings/settings-list";
import { SettingsRow, SettingsSection } from "#/components/settings/settings-section";
import { Button } from "#/components/ui/button";
import { Checkbox } from "#/components/ui/checkbox";
import { OPERATE_REFUSED } from "#/modules/machines/server-policy.hooks";
import { bytesText } from "#/modules/metrics/metrics-summary";
import { useRoleGate } from "#/modules/organization/members.queries";
import { approvedTrash } from "#/modules/trash/trash-approval";
import { emptyServerTrashServerFn, restoreServerTrashServerFn } from "#/modules/trash/trash.functions";
import { serverTrash, serverTrashKey } from "#/modules/trash/trash.queries";

const KIND = { root: "Volume", slot: "Mirror or old copy", copy: "Mirror's old data" } as const;

const entries = (count: number) => (count === 1 ? "1 entry" : `${count} entries`);

const when = (unixSeconds: number) =>
  new Date(unixSeconds * 1000).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" });

/** What an entry is, its size, when it was deleted and when the Trash deletes it for good. */
function entryHint(entry: TrashEntry) {
  const kind = entry.restoring ? `${KIND[entry.place]}, being restored` : KIND[entry.place];
  return `${kind} · ${bytesText(entry.used_bytes)} · Deleted ${when(entry.trashed_at)} · Gone for good ${when(entry.expires_at)}`;
}

/**
 * The deleted Volume data this Server keeps for 7 days. A person picks entries and restores them or deletes them for
 * good, each time with their passkey, which approves exactly those entries on this Server and nothing else.
 */
export function ServerTrashSection({ organizationSlug, server }: { organizationSlug: string; server: { id: string; name: string } }) {
  const { data } = useQuery(serverTrash(organizationSlug, server.id));
  if (data === undefined) return null;
  if (data.kind === "unread") {
    return (
      <SettingsSection id="server-trash" title="Trash" description={DESCRIPTION}>
        <SettingsRow label="Couldn't read the trash" hint={data.message} commit="none" />
      </SettingsSection>
    );
  }
  return <TrashEntries organizationSlug={organizationSlug} server={server} trash={data.trash} />;
}

const DESCRIPTION = "Deleted volume data this server keeps for 7 days. Your passkey approves exactly the entries you pick.";

function TrashEntries({ organizationSlug, server, trash }: { organizationSlug: string; server: { id: string; name: string }; trash: TrashList }) {
  const queryClient = useQueryClient();
  const gate = useRoleGate(organizationSlug, "servers:operate", OPERATE_REFUSED);
  const [picked, setPicked] = useState<ReadonlySet<string>>(new Set());
  const [emptying, setEmptying] = useState(false);
  const chosen = trash.entries.filter((entry) => picked.has(entry.id));
  const ids = chosen.map((entry) => entry.id);
  const settle = () => {
    setPicked(new Set());
    return queryClient.invalidateQueries({ queryKey: serverTrashKey(organizationSlug, server.id) });
  };
  const restore = useMutation({
    mutationFn: async () => restoreServerTrashServerFn({
      data: { organizationSlug, machineId: server.id, approval: await approvedTrash(queryClient, organizationSlug, server.id, "restore", ids) },
    }),
    onSuccess: async ({ restored, failed }) => {
      if (restored.length > 0) toast(`Restored ${entries(restored.length)} on ${server.name}`);
      for (const failure of failed) toast.error(`${failure.id} wasn't restored`, { description: failure.reason });
      await settle();
    },
  });
  const toggle = (id: string, on: boolean) => setPicked((current) => {
    const next = new Set(current);
    if (on) next.add(id); else next.delete(id);
    return next;
  });
  const busy = restore.isPending || emptying;
  const idle = ids.length === 0 || !gate.allowed || busy;
  const empty = trash.entries.length === 0 && trash.unreadable.length === 0;
  return (
    <SettingsSection
      id="server-trash"
      title="Trash"
      description={DESCRIPTION}
      action={empty ? undefined : (
        <>
          <Button variant="outline" size="sm" disabled={idle} title={gate.reason} onClick={() => restore.mutate()}>
            <ArchiveRestoreIcon data-icon="inline-start" />
            {restore.isPending ? "Waiting for your passkey…" : ids.length === 0 ? "Restore" : `Restore ${entries(ids.length)}`}
          </Button>
          <Button variant="destructive" size="sm" disabled={idle} title={gate.reason} onClick={() => setEmptying(true)}>
            <Trash2Icon data-icon="inline-start" />
            Delete for good
          </Button>
        </>
      )}
    >
      {restore.isError ? (
        <SettingsRow label="Nothing restored" hint={<span role="alert">{restore.error.message}</span>} commit="none" />
      ) : null}
      {empty ? <SettingsEmpty title="Nothing in the trash" /> : (
        <SettingsList label={`${server.name}'s trash`}>
          {trash.entries.map((entry) => (
            <SettingsListItem
              key={entry.id}
              pick
              media={<HardDriveIcon />}
              label={<span className="font-mono">{entry.name}</span>}
              hint={entryHint(entry)}
              commit="none"
              actions={(
                <Checkbox
                  checked={picked.has(entry.id)}
                  disabled={busy}
                  onCheckedChange={(on) => toggle(entry.id, on)}
                />
              )}
            />
          ))}
          {trash.unreadable.map((entry) => (
            <SettingsListItem
              key={entry.id}
              media={<FileQuestionIcon />}
              label={<span className="font-mono">{entry.id}</span>}
              hint={`Ployz can't read this entry, so only root on ${server.name} can delete it · ${bytesText(entry.used_bytes)} · ${entry.reason}`}
              commit="none"
            />
          ))}
        </SettingsList>
      )}
      <DeletionDialog
        open={emptying}
        onOpenChange={setEmptying}
        title={`Delete ${entries(chosen.length)} for good?`}
        place={server.name}
        confirmLabel="Delete for good"
        pendingLabel="Waiting for your passkey"
        sentence={<>This deletes from <span className="font-medium">{server.name}</span>&rsquo;s trash, and can&rsquo;t be undone:</>}
        callbacks={{
          load: async () => ({ items: chosen.map((entry) => ({ kind: "volume" as const, name: entry.name, detail: bytesText(entry.used_bytes) })), evidence: ids }),
          confirm: async (named) => {
            const approval = await approvedTrash(queryClient, organizationSlug, server.id, "empty", named);
            const emptied = await emptyServerTrashServerFn({ data: { organizationSlug, machineId: server.id, approval } });
            await settle();
            if (emptied.failed.length > 0) throw new Error(emptied.failed.map((failure) => `${failure.id}: ${failure.reason}`).join(" "));
            toast(`Deleted ${entries(emptied.emptied.length)} from ${server.name}'s trash`);
          },
        }}
      />
    </SettingsSection>
  );
}
