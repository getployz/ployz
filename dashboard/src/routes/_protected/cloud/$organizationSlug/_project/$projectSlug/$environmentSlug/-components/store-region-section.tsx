import { FieldError } from "#/components/ui/field";
import { SettingsRow, SettingsSection } from "#/components/settings/settings-section";
import { useCommitHold } from "#/components/store-write/use-commit-hold";
import { RegionSelect } from "#/modules/machines/RegionSelect";
import { useRegionOptions } from "#/modules/machines/server-regions.hooks";
import { environmentSettingsQuery, useStoreView } from "#/modules/config-store/store-view.queries";
import { useStoreWriter } from "#/modules/config-store/store-write";

const REGION_PATH = "@environment.region";

/** Where this Environment's Services run unless a Service names regions of its own. Staged until the next Deploy, like any Setting. */
export function StoreRegionSection({ organizationSlug, projectSlug, environmentSlug }: {
  organizationSlug: string;
  projectSlug: string;
  environmentSlug: string;
}) {
  const environment = { project: projectSlug, environment: environmentSlug };
  const view = useStoreView(organizationSlug, environmentSettingsQuery(environment));
  const writer = useStoreWriter(organizationSlug);
  const saved = view.ok ? view.value.region ?? null : null;
  const [region, hold, state] = useCommitHold<string | null>(saved);
  const options = useRegionOptions(organizationSlug, region === null ? [] : [region]);
  if (!view.ok) return null;
  return (
    <SettingsSection id="region" title="Region">
      <SettingsRow label="Runs in" commit="change"
        control={(
          <RegionSelect label="Runs in" options={options} value={region}
            onChange={(next) => hold(next, writer.edit({ environment, changes: [
              next === null ? { op: "unset", path: REGION_PATH } : { op: "set", path: REGION_PATH, value: next },
            ] }))} />
        )}>
        {state.phase === "failed" ? <FieldError>{state.reason}</FieldError> : null}
      </SettingsRow>
    </SettingsSection>
  );
}
