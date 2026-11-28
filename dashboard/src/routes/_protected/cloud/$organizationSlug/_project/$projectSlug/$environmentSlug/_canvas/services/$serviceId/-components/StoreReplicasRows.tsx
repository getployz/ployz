import type { ReactNode } from "react";
import { Link, useHydrated } from "@tanstack/react-router";
import { MinusIcon, PlusIcon, XIcon } from "lucide-react";
import type { JsonValue, VolumeListing } from "@ployz/sdk";
import { InfoHint } from "#/components/info-hint";
import { SHARED_VOLUME_WHY } from "#/modules/volume-placement/volume-placement";
import { SettingsRow } from "#/components/settings/settings-section";
import { useCommitHold } from "#/components/store-write/use-commit-hold";
import { Button } from "#/components/ui/button";
import { FieldError } from "#/components/ui/field";
import { REPLICAS_MAX, replicaRows, replicaValue, type ReplicaRow } from "#/modules/config-store/replica-rows";
import type { Write } from "#/modules/config-store/store-write";
import type { SharedRegion } from "#/modules/config-store/deploy-waits";
import { RegionSelect } from "#/modules/machines/RegionSelect";
import type { RegionOption } from "#/modules/machines/server-regions";
import { ENVIRONMENT_RESOURCE_ROUTE_TO } from "#/lib/environment-routes";

/**
 * A Service's replicas as a row per region: how many run where. A bare count is one row in the Environment's region, or
 * in any region when it has none. A Volume that doesn't allow shared writes fixes the count at 1 and leaves the region to
 * pick. A second region is on offer only where the Store takes one: the Environment has a region, and the Service
 * mounts no Volume. A Service that shares a Volume runs only in the Volume's region. Whether a count names its region
 * follows the value as it is now, wherever it was set. Until the page can act on a click, every control shows disabled,
 * so none is lost.
 */
export function StoreReplicasRows({ options, value, deployed, home, capped, oneRegion, sharing, changed, onChange }: {
  options: readonly RegionOption[];
  /** The `replicas` value as the next Deploy has it. */
  value: JsonValue;
  /** The value the Service last ran with. */
  deployed: JsonValue;
  /** The Environment's region, which a bare count runs in. */
  home: string | null;
  /** What says why the count is held to 1, when a Volume holds it. */
  capped?: ReactNode;
  /** Whether a Volume the Service mounts keeps it to one region: any Volume does, as the Store rules. */
  oneRegion: boolean;
  /** The region the Volume it shares is in, which is the only one it can run in. */
  sharing: SharedRegion | null;
  changed: boolean;
  onChange: (value: JsonValue) => Write;
}) {
  const live = useHydrated();
  const [rows, hold, commit] = useCommitHold<readonly ReplicaRow[]>(replicaRows(value, home));
  const save = (next: readonly ReplicaRow[]) => hold(next, onChange(replicaValue(next, home, value, deployed)));
  const update = (index: number, change: Partial<ReplicaRow>) => save(rows.map((row, at) => (at === index ? { ...row, ...change } : row)));
  const unused = options.find(({ key, servers }) => key !== null && servers.length > 0 && !rows.some(({ region }) => region === key));
  const total = rows.reduce((sum, { count }) => sum + count, 0);
  const full = total >= REPLICAS_MAX;
  const canAdd = home !== null && !oneRegion && !full && unused !== undefined;
  const taken = rows.flatMap(({ region }) => region ?? []);
  const nameOf = (region: string | null) => options.find(({ key }) => key === region)?.name ?? region;
  const offered = sharing === null ? options : options.filter(({ key }) => key === sharing.region);
  const hint = sharing === null ? capped : (
    <>
      Must run in {nameOf(sharing.region)}. It shares the Volume <strong>{sharing.volume}</strong> with {allOf(sharing.sharers)}
      {sharing.located ? <>, and <strong>{sharing.volume}</strong> is in {nameOf(sharing.region)}.</> : <>, so they have to run in one region.</>}
      {capped === undefined ? null : <> {capped}</>}
    </>
  );
  return (
    <SettingsRow label="Replicas" commit="change" hint={hint}
      control={(
        <div className="flex flex-col items-end gap-2">
          {rows.map((row, index) => {
            const where = nameOf(row.region);
            return (
              <div key={index} className="flex items-center gap-2">
                <RegionSelect label={`Region of row ${index + 1}`} options={offered} value={row.region} taken={taken}
                  allowAny={rows.length === 1 && home === null && sharing === null} changed={changed}
                  disabled={!live || (sharing !== null && row.region === sharing.region)}
                  onChange={(region) => update(index, { region })} />
                <Stepper label={where === null ? "Replicas" : `Replicas in ${where}`} count={row.count} changed={changed}
                  canAdd={!full} disabled={!live || capped !== undefined}
                  onChange={(count) => update(index, { count })} />
                {rows.length > 1 ? (
                  <Button type="button" variant="ghost" size="icon" aria-label={`Remove ${where ?? "region"}`} disabled={!live}
                    onClick={() => save(rows.filter((_, at) => at !== index))}>
                    <XIcon />
                  </Button>
                ) : null}
              </div>
            );
          })}
          {canAdd ? (
            <Button type="button" variant="outline" size="sm" disabled={!live} onClick={() => save([...rows, { region: unused.key, count: 1 }])}>
              <PlusIcon data-icon="inline-start" />
              Add region
            </Button>
          ) : null}
        </div>
      )}>
      {commit.phase === "failed" ? <FieldError>{commit.reason}</FieldError> : null}
    </SettingsRow>
  );
}

const allOf = (names: readonly string[]) => (names.length < 2 ? names.join("") : `${names.slice(0, -1).join(", ")} and ${names.at(-1)}`);

export function CappedHint({ params, volume }: {
  params: { organizationSlug: string; projectSlug: string; environmentSlug: string };
  volume: VolumeListing;
}) {
  return (
    <>
      Limited to 1 while <strong>{volume.name}</strong> is attached.{" "}
      <span className="inline-flex align-middle"><InfoHint>{SHARED_VOLUME_WHY}</InfoHint></span>{" "}
      <Link className="whitespace-nowrap" to={ENVIRONMENT_RESOURCE_ROUTE_TO} params={{ ...params, resourceId: volume.id }}>Allow shared writes</Link>
    </>
  );
}

function Stepper({ label, count, canAdd, changed, disabled, onChange }: {
  label: string;
  count: number;
  /** Whether one more would stay within the most a Service runs. */
  canAdd: boolean;
  changed: boolean;
  disabled: boolean;
  onChange: (count: number) => void;
}) {
  return (
    <div role="group" aria-label={label} className="flex items-center gap-1">
      <Button type="button" variant="outline" size="icon" aria-label="One fewer" disabled={disabled || count <= 1} onClick={() => onChange(count - 1)}>
        <MinusIcon />
      </Button>
      <output data-changed={changed || undefined}
        className="flex h-8 min-w-10 items-center justify-center rounded-lg border border-input px-2 text-sm tabular-nums data-[changed=true]:border-changed-border data-[changed=true]:bg-changed-soft">
        {count}
      </output>
      <Button type="button" variant="outline" size="icon" aria-label="One more" disabled={disabled || !canAdd} onClick={() => onChange(count + 1)}>
        <PlusIcon />
      </Button>
    </div>
  );
}
