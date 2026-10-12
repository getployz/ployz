import { useId } from "react";
import { Field, FieldContent, FieldDescription, FieldGroup, FieldLabel } from "#/components/ui/field";
import { Switch } from "#/components/ui/switch";
import type { SupportAccessChange } from "#/modules/support-access/support-access";
import { useSupportAccess } from "#/modules/support-access/support-access.hooks";
import { SettingsSection } from "#/routes/_protected/cloud/$organizationSlug/-components/SettingsSection";

const SHOWS = [
  { setting: "variables", label: "Variables", description: "Values of variables that aren't sealed." },
  { setting: "configs", label: "Config files", description: "What's in each config file." },
  { setting: "logs", label: "Container logs", description: "What your containers print." },
] as const;

/** "Allow support access": whether Ployz support may open this Organization and act in it, and what it may read there. */
export function SupportAccessSection({ organizationSlug }: { organizationSlug: string }) {
  const { row, change } = useSupportAccess(organizationSlug);
  return (
    <SettingsSection id="support-access" title="Support">
      <SupportSwitch
        label="Allow support access"
        description="Let Ployz support open this Organization and act in it."
        checked={row.allowed}
        onChange={(on) => change({ setting: "allowed", on })}
      />
      {row.allowed ? (
        <FieldGroup className="border-l pl-4">
          {SHOWS.map(({ setting, label, description }) => (
            <SupportSwitch
              key={setting}
              label={label}
              description={description}
              checked={row[setting]}
              onChange={(on) => change({ setting, on } satisfies SupportAccessChange)}
            />
          ))}
        </FieldGroup>
      ) : null}
    </SettingsSection>
  );
}

function SupportSwitch({ label, description, checked, onChange }: {
  label: string;
  description: string;
  checked: boolean;
  onChange: (on: boolean) => void;
}) {
  const switchId = useId();
  return (
    <Field orientation="horizontal">
      <FieldContent>
        <FieldLabel htmlFor={switchId}>{label}</FieldLabel>
        <FieldDescription>{description}</FieldDescription>
      </FieldContent>
      <Switch id={switchId} checked={checked} onCheckedChange={onChange} />
    </Field>
  );
}
