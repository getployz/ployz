import { useId } from "react";
import { createOptimisticAction, useLiveSuspenseQuery } from "@tanstack/react-db";
import { useLoaderData } from "@tanstack/react-router";
import { toast } from "sonner";
import { getServerUpgradeSettingsCollection } from "#/collections/collections";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { Button } from "#/components/ui/button";
import { Dialog, DialogClose, DialogContent, DialogFooter, DialogHeader, DialogTitle } from "#/components/ui/dialog";
import { Field, FieldContent, FieldDescription, FieldLabel } from "#/components/ui/field";
import { Switch } from "#/components/ui/switch";
import { automaticUpgrades } from "#/modules/server-upgrade/server-upgrade";
import { setAutomaticServerUpgradesServerFn } from "#/modules/server-upgrade/server-upgrade.functions";

/** The Organization's "Upgrade automatically" setting: shown at once, saved in the background, rolled back with a toast. */
export function useAutomaticUpgrades(organizationSlug: string) {
  const collection = getServerUpgradeSettingsCollection(organizationSlug, useCollectionScope());
  const { data: rows } = useLiveSuspenseQuery(collection);
  const { organizationId } = useLoaderData({ from: "/_protected/cloud/$organizationSlug" });
  const set = createOptimisticAction<boolean>({
    onMutate: (automatic) => {
      if (collection.has(organizationId)) collection.update(organizationId, (row) => { row.automatic = automatic; });
      else collection.insert({ id: organizationId, automatic });
    },
    mutationFn: (automatic) => setAutomaticServerUpgradesServerFn({ data: { organizationSlug, automatic } })
      .then((row) => collection.writeCommitted(row)),
  });
  return {
    automatic: automaticUpgrades(rows),
    setAutomatic: (automatic: boolean) => {
      set(automatic).isPersisted.promise.catch(() => toast.error("Could not change automatic upgrades"));
    },
  };
}

/** "Server upgrades": whether Servers upgrade themselves. Opened from the Servers page's upgrade line. */
export function ServerUpgradesDialog({ organizationSlug, open, onOpenChange }: {
  organizationSlug: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const { automatic, setAutomatic } = useAutomaticUpgrades(organizationSlug);
  const switchId = useId();
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
        <DialogFooter>
          <DialogClose render={<Button />}>Done</DialogClose>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
