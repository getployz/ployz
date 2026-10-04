import { useId } from "react";
import { createOptimisticAction, useLiveSuspenseQuery } from "@tanstack/react-db";
import { useLoaderData } from "@tanstack/react-router";
import { toast } from "sonner";
import { getServerUpgradeSettingsCollection } from "#/collections/collections";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { Button } from "#/components/ui/button";
import { Dialog, DialogClose, DialogContent, DialogFooter, DialogHeader, DialogTitle } from "#/components/ui/dialog";
import { Field, FieldContent, FieldDescription, FieldLabel } from "#/components/ui/field";
import { Select, SelectContent, SelectGroup, SelectItem, SelectTrigger, SelectValue } from "#/components/ui/select";
import { Switch } from "#/components/ui/switch";
import { RELEASE_CHANNELS, type ReleaseChannel, serverUpgradeSettings } from "#/modules/server-upgrade/server-upgrade";
import { setServerUpgradeSettingsServerFn } from "#/modules/server-upgrade/server-upgrade.functions";

type Change = { readonly automatic: boolean } | { readonly channel: ReleaseChannel };
const CHANNEL_LABELS: Record<ReleaseChannel, string> = { stable: "Stable", beta: "Beta" };

/**
 * The Organization's Server upgrade settings: "Upgrade automatically" and its Release Channel. A change shows at once,
 * saves in the background, and rolls back with a toast.
 */
export function useServerUpgradeSettings(organizationSlug: string) {
  const collection = getServerUpgradeSettingsCollection(organizationSlug, useCollectionScope());
  const { data: rows } = useLiveSuspenseQuery(collection);
  const { organizationId } = useLoaderData({ from: "/_protected/cloud/$organizationSlug" });
  const settings = serverUpgradeSettings(rows);
  const set = createOptimisticAction<Change>({
    onMutate: (change) => {
      if (collection.has(organizationId)) collection.update(organizationId, (row) => { Object.assign(row, change); });
      else collection.insert({ id: organizationId, ...settings, ...change });
    },
    mutationFn: (change) => setServerUpgradeSettingsServerFn({ data: { organizationSlug, ...change } })
      .then((row) => collection.writeCommitted(row)),
  });
  const save = (change: Change, failure: string) => {
    set(change).isPersisted.promise.catch(() => toast.error(failure));
  };
  return {
    ...settings,
    setAutomatic: (automatic: boolean) => save({ automatic }, "Could not change automatic upgrades"),
    setChannel: (channel: ReleaseChannel) => save({ channel }, "Could not change releases"),
  };
}

/** "Server upgrades": whether Servers upgrade themselves, and to Stable or Beta releases. Opened from the Servers page's upgrade line. */
export function ServerUpgradesDialog({ organizationSlug, open, onOpenChange }: {
  organizationSlug: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const { automatic, setAutomatic, channel, setChannel } = useServerUpgradeSettings(organizationSlug);
  const switchId = useId();
  const channelId = useId();
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Server upgrades</DialogTitle>
        </DialogHeader>
        <Field orientation="horizontal">
          <FieldContent>
            <FieldLabel htmlFor={switchId}>Upgrade automatically</FieldLabel>
            <FieldDescription>Your apps keep running while servers upgrade.</FieldDescription>
          </FieldContent>
          <Switch id={switchId} checked={automatic} onCheckedChange={setAutomatic} />
        </Field>
        <Field orientation="horizontal">
          <FieldContent>
            <FieldLabel htmlFor={channelId}>Releases</FieldLabel>
          </FieldContent>
          <Select value={channel} onValueChange={(value) => { if (value !== null && value !== channel) setChannel(value); }}>
            <SelectTrigger id={channelId}><SelectValue>{CHANNEL_LABELS[channel]}</SelectValue></SelectTrigger>
            <SelectContent>
              <SelectGroup>
                {RELEASE_CHANNELS.map((value) => (
                  <SelectItem key={value} value={value} label={CHANNEL_LABELS[value]}>{CHANNEL_LABELS[value]}</SelectItem>
                ))}
              </SelectGroup>
            </SelectContent>
          </Select>
        </Field>
        <DialogFooter>
          <DialogClose render={<Button />}>Done</DialogClose>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
