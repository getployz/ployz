import { BoxIcon, CircleCheckIcon, CircleDashedIcon, CircleMinusIcon, CircleStopIcon, CircleXIcon, GlobeIcon, type LucideIcon } from "lucide-react";
import { plural } from "#/lib/plural";
import { RelativeTime } from "#/components/relative-time";
import { SettingsList, SettingsListItem } from "#/components/settings/settings-list";
import { SettingsRow, SettingsSection } from "#/components/settings/settings-section";
import { Button } from "#/components/ui/button";
import { Spinner } from "#/components/ui/spinner";
import { Switch } from "#/components/ui/switch";
import { drainBusy, drainButtonLabel, type DrainRow, type DrainTone, type DrainView } from "#/modules/machines/server-drain-view";
import { useServerPolicy } from "#/modules/machines/server-policy.hooks";
import type { RuntimeMachineRecord } from "#/modules/runtime/runtime.collection";
import { useStagedOffServer } from "#/modules/volume-placement/volume-placement.hooks";

/** Whether this Server takes Services, and Drain. Off alone is a cordon: nothing new lands here, nothing moves. */
export function ServerServicesSection({ machine, organizationSlug, view, left, onDrain, unavailable }: {
  machine: RuntimeMachineRecord;
  organizationSlug: string;
  view: DrainView;
  /** What a Drain would still move off here (`drainLeftRows`). */
  left: readonly DrainRow[];
  onDrain: () => void;
  /** Why Drain can't run now (the Server is offline, or the page can't see it); null when it can. */
  unavailable: string | null;
}) {
  const policy = useServerPolicy(machine, organizationSlug, "services setting");
  const busy = drainBusy(view);
  const staged = useStagedOffServer(organizationSlug, machine.id);
  return (
    <ServerServicesRows
      acceptsServices={policy.acceptsServices}
      onAcceptsServices={(acceptsServices) => policy.request({ acceptsServices })}
      view={view}
      left={left}
      onDrain={onDrain}
      unavailable={busy ? null : unavailable}
      staged={staged}
    />
  );
}

/**
 * The Services section as it renders: the switch, the Drain row, and the latest Drain's result. While a Drain is under
 * way it lists what it still has to move, each Service leaving the list as it leaves this Server. Volumes move with the
 * next Deploy, so the row counts those staged to leave.
 */
export function ServerServicesRows({ acceptsServices, onAcceptsServices, view, left, onDrain, unavailable, staged }: {
  acceptsServices: boolean;
  onAcceptsServices: (acceptsServices: boolean) => void;
  view: DrainView;
  left: readonly DrainRow[];
  onDrain: () => void;
  unavailable: string | null;
  /** Volume primaries here staged to move off. */
  staged: number;
}) {
  const busy = drainBusy(view);
  return (
    <SettingsSection id="server-services" title="Services">
      <SettingsRow
        label="Run services here"
        hint={acceptsServices ? undefined : "Nothing new starts here. What runs here stays until you drain."}
        commit="change"
        control={(
          // Turning it on mid-drain would leave the rest of the Drain nothing to move: it waits for the Drain.
          <Switch aria-label="Run services here" checked={acceptsServices} disabled={busy} onCheckedChange={(checked) => onAcceptsServices(checked)} />
        )}
      />
      <SettingsRow
        label="Drain"
        hint={unavailable ?? <DrainLine view={view} left={left.length} />}
        commit="none"
        control={<DrainButton view={view} onClick={onDrain} disabled={unavailable !== null} />}
      >
        {view.kind === "finished" && view.stoppedEarly !== null ? (
          <p className="text-sm text-warning">It stopped early: {view.stoppedEarly}.</p>
        ) : null}
        {staged > 0 ? <p className="text-sm text-muted-foreground">Next deploy moves {plural(staged, "volume")}</p> : null}
      </SettingsRow>
      {busy ? <DrainRows rows={left} label="Left to move" /> : view.kind === "finished" ? <DrainRows rows={view.rows} label="Drain result" /> : null}
    </SettingsSection>
  );
}

const TONE_CLASS = {
  moved: "text-success",
  stopped: "text-muted-foreground",
  stayed: "text-warning",
  failed: "text-destructive",
  neutral: "text-muted-foreground",
} satisfies Record<DrainTone, string>;
const TONE_ICON = {
  moved: CircleCheckIcon,
  stopped: CircleStopIcon,
  stayed: CircleMinusIcon,
  failed: CircleXIcon,
  neutral: CircleDashedIcon,
} satisfies Record<DrainTone, LucideIcon>;

/** Each Service and what the latest Drain did with it, or what the one under way still has to move. */
function DrainRows({ rows, label }: { rows: readonly DrainRow[]; label: string }) {
  if (rows.length === 0) return null;
  return (
    <SettingsList label={label}>
      {rows.map((row) => {
        const Icon = TONE_ICON[row.tone];
        return (
          <SettingsListItem
            key={row.key}
            media={row.global ? <GlobeIcon /> : <BoxIcon />}
            label={row.name}
            hint={<span className={row.tone === "failed" ? "text-destructive" : undefined}>{row.reason ?? row.namespace}</span>}
            commit="none"
            actions={(
              <span className={`flex items-center gap-2 ${TONE_CLASS[row.tone]}`}>
                <Icon aria-hidden className="size-4 shrink-0" />
                {row.label}
              </span>
            )}
          />
        );
      })}
    </SettingsList>
  );
}

/** The Drain row's words: what it does, or how the latest one went. */
function DrainLine({ view, left }: { view: DrainView; left: number }) {
  switch (view.kind) {
    case "idle":
      return <>Move all services to your other servers.</>;
    case "starting":
      return <>Starting…</>;
    case "queued":
      return view.behind === null ? <>Queued</> : <>Queued · Waiting on {view.behind}</>;
    case "running":
      return <>Draining · {left === 0 ? "Finishing up" : `${left} left`} · Started <RelativeTime date={new Date(view.since)} /></>;
    case "finished":
      return <>Drained <RelativeTime date={new Date(view.at)} /> · {view.summary}</>;
    case "failed":
      return <>{view.words}{view.details === null ? null : <>: {view.details}</>}</>;
    case "cancelled":
      return <>Drain was cancelled <RelativeTime date={new Date(view.at)} />, before it started.</>;
    case "unknown":
      return <>{view.words}</>;
  }
}

/** Drain | Drain again | a spinner while one is under way. */
export function DrainButton({ view, onClick, size, disabled = false }: {
  view: DrainView;
  onClick: () => void;
  size?: "sm";
  disabled?: boolean;
}) {
  return drainBusy(view) ? (
    <Button variant="outline" size={size} disabled>
      <Spinner data-icon="inline-start" />
      Draining…
    </Button>
  ) : (
    <Button variant="outline" size={size} disabled={disabled} onClick={onClick}>{drainButtonLabel(view)}</Button>
  );
}
