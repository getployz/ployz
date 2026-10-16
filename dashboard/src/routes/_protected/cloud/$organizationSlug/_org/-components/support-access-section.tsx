import { useId } from "react";
import { useQuery } from "@tanstack/react-query";
import { SettingsField, SettingsGroup } from "#/components/settings/settings-group";
import { SettingsSection } from "#/components/settings/settings-section";
import { Switch } from "#/components/ui/switch";
import { organizationMembersQueryOptions } from "#/modules/organization/members.queries";
import type { SupportAccessChange } from "#/modules/support-access/support-access";
import { useSupportAccess } from "#/modules/support-access/support-access.hooks";

const SHOWS = [
  { setting: "variables", label: "Variables" },
  { setting: "configs", label: "Config files" },
  { setting: "logs", label: "Container logs" },
] as const;

/** "Allow support access": whether Ployz support may open this Organization and act in it, and what it may read there. */
export function SupportAccessSection({ organizationSlug }: { organizationSlug: string }) {
  const { row, change } = useSupportAccess(organizationSlug);
  // Only an owner or admin decides what Ployz support may do. Everyone else, support included, sees the switches but can't move them.
  const yourRole = useQuery(organizationMembersQueryOptions(organizationSlug)).data?.members.find((row) => row.you)?.role;
  const locked = yourRole !== "owner" && yourRole !== "admin";
  return (
    <SettingsSection id="support-access" title="Support">
      <SettingsGroup
        label="Allow support access"
        hint={row.allowed ? "Let Ployz support open this Organization and act in it, and read what's on below. Sealed variables stay hidden." : "Let Ployz support open this Organization and act in it."}
        commit="change"
        control={(
          <Switch
            aria-label="Allow support access"
            checked={row.allowed}
            disabled={locked}
            onCheckedChange={(on) => change({ setting: "allowed", on })}
          />
        )}
      >
        {row.allowed ? SHOWS.map(({ setting, label }) => (
          <SupportField
            key={setting}
            label={label}
            checked={row[setting]}
            disabled={locked}
            onChange={(on) => change({ setting, on } satisfies SupportAccessChange)}
          />
        )) : null}
      </SettingsGroup>
    </SettingsSection>
  );
}

function SupportField({ label, checked, disabled, onChange }: {
  label: string;
  checked: boolean;
  disabled: boolean;
  onChange: (on: boolean) => void;
}) {
  const switchId = useId();
  return (
    <SettingsField
      label={label}
      htmlFor={switchId}
      commit="change"
      control={<Switch id={switchId} aria-label={label} checked={checked} disabled={disabled} onCheckedChange={onChange} />}
    />
  );
}
