import { useId } from "react";
import { Field, FieldContent, FieldDescription, FieldLabel } from "#/components/ui/field";
import { Switch } from "#/components/ui/switch";
import { useSupportAccess } from "#/modules/support-access/support-access.hooks";
import { SettingsSection } from "#/routes/_protected/cloud/$organizationSlug/-components/SettingsSection";

/** "Allow support access": whether Ployz support may open this Organization and act in it. */
export function SupportAccessSection({ organizationSlug }: { organizationSlug: string }) {
  const { allowed, setAllowed } = useSupportAccess(organizationSlug);
  const switchId = useId();
  return (
    <SettingsSection id="support-access" title="Support">
      <Field orientation="horizontal">
        <FieldContent>
          <FieldLabel htmlFor={switchId}>Allow support access</FieldLabel>
          <FieldDescription>Let Ployz support open this Organization and act in it.</FieldDescription>
        </FieldContent>
        <Switch id={switchId} checked={allowed} onCheckedChange={setAllowed} />
      </Field>
    </SettingsSection>
  );
}
