import { useId } from "react";
import { Button } from "#/components/ui/button";
import { Dialog, DialogClose, DialogContent, DialogFooter, DialogHeader, DialogTitle } from "#/components/ui/dialog";
import { Field, FieldContent, FieldDescription, FieldLabel } from "#/components/ui/field";
import { Select, SelectContent, SelectGroup, SelectItem, SelectTrigger, SelectValue } from "#/components/ui/select";
import { Switch } from "#/components/ui/switch";
import { RELEASE_CHANNELS, type ReleaseChannel } from "#/modules/server-upgrade/server-upgrade";
import { useServerUpgradeSettings } from "#/modules/server-upgrade/server-upgrade.hooks";

const CHANNEL_LABELS = { stable: "Stable", beta: "Beta" } satisfies Record<ReleaseChannel, string>;

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
