import { useId } from "react";
import { useQuery } from "@tanstack/react-query";
import { Field, FieldContent, FieldDescription, FieldGroup, FieldLabel } from "#/components/ui/field";
import { Switch } from "#/components/ui/switch";
import { organizationMembersQueryOptions } from "#/modules/organization/members.queries";
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
  // Only an owner or admin decides what Ployz support may do. Everyone else, support included, sees the switches but can't move them.
  const yourRole = useQuery(organizationMembersQueryOptions(organizationSlug)).data?.members.find((row) => row.you)?.role;
  const locked = yourRole !== "owner" && yourRole !== "admin";
  return (
    <SettingsSection id="support-access" title="Support">
      <SupportSwitch
        label="Allow support access"
        description="Let Ployz support open this Organization and act in it."
        checked={row.allowed}
        disabled={locked}
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
              disabled={locked}
              onChange={(on) => change({ setting, on } satisfies SupportAccessChange)}
            />
          ))}
        </FieldGroup>
      ) : null}
    </SettingsSection>
  );
}

function SupportSwitch({ label, description, checked, disabled, onChange }: {
  label: string;
  description: string;
  checked: boolean;
  disabled: boolean;
  onChange: (on: boolean) => void;
}) {
  const switchId = useId();
  return (
    <Field orientation="horizontal">
      <FieldContent>
        <FieldLabel htmlFor={switchId}>{label}</FieldLabel>
        <FieldDescription>{description}</FieldDescription>
      </FieldContent>
      <Switch id={switchId} aria-label={label} checked={checked} disabled={disabled} onCheckedChange={onChange} />
    </Field>
  );
}
