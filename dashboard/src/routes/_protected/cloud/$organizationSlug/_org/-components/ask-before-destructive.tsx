import { useId } from "react";
import { Field, FieldContent, FieldDescription, FieldLabel } from "#/components/ui/field";
import { Switch } from "#/components/ui/switch";
import { useAskBeforeDestructive } from "#/modules/approvals/approvals.hooks";
import { SettingsSection } from "#/routes/_protected/cloud/$organizationSlug/-components/SettingsSection";

export function AskBeforeDestructive({ organizationSlug }: { organizationSlug: string }) {
  const { askBeforeDestructive, setAskBeforeDestructive } = useAskBeforeDestructive(organizationSlug);
  const switchId = useId();
  return (
    <SettingsSection id="approvals" title="Approvals">
      <Field orientation="horizontal">
        <FieldContent>
          <FieldLabel htmlFor={switchId}>Ask before destructive actions</FieldLabel>
          <FieldDescription>
            When the CLI or a coding agent publishes, deploys, removes a server, drains one or cleans up a namespace
            in a way that deletes a service, a volume or its data, someone in this organization approves it first.
            Changes you make here never ask.
          </FieldDescription>
        </FieldContent>
        <Switch id={switchId} checked={askBeforeDestructive} onCheckedChange={setAskBeforeDestructive} />
      </Field>
    </SettingsSection>
  );
}
