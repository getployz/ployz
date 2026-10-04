import { useId } from "react";
import { RelativeTime } from "#/components/relative-time";
import { Button } from "#/components/ui/button";
import { Field, FieldContent, FieldDescription, FieldLabel, FieldTitle } from "#/components/ui/field";
import { Spinner } from "#/components/ui/spinner";
import { Switch } from "#/components/ui/switch";
import { drainBusy, drainButtonLabel, type DrainView } from "#/modules/machines/server-drain-view";
import { useServerPolicy } from "#/modules/machines/server-policy.hooks";
import type { RuntimeMachineRecord } from "#/modules/runtime/runtime.collection";
import { SettingsSection } from "#/routes/_protected/cloud/$organizationSlug/-components/SettingsSection";
import { DrainResultList } from "./drain-result";

/** Whether this Server takes Services, and Drain. Off alone is a cordon: nothing new lands here, nothing moves. */
export function ServerServicesSection({ machine, organizationSlug, view, onDrain, unavailable }: {
  machine: RuntimeMachineRecord;
  organizationSlug: string;
  view: DrainView;
  onDrain: () => void;
  /** Why Drain can't run now (the Server is offline, or the page can't see it); null when it can. */
  unavailable: string | null;
}) {
  const policy = useServerPolicy(machine, organizationSlug, "services setting");
  const busy = drainBusy(view);
  return (
    <ServerServicesRows
      acceptsServices={policy.acceptsServices}
      onAcceptsServices={(acceptsServices) => policy.request({ acceptsServices })}
      view={view}
      onDrain={onDrain}
      unavailable={busy ? null : unavailable}
    />
  );
}

/** The Services section as it renders: the switch, the Drain row, and the latest Drain's result. */
export function ServerServicesRows({ acceptsServices, onAcceptsServices, view, onDrain, unavailable }: {
  acceptsServices: boolean;
  onAcceptsServices: (acceptsServices: boolean) => void;
  view: DrainView;
  onDrain: () => void;
  unavailable: string | null;
}) {
  const switchId = useId();
  const busy = drainBusy(view);
  return (
    <SettingsSection id="server-services" title="Services">
      <Field orientation="horizontal">
        <FieldContent>
          <FieldLabel htmlFor={switchId}>Run services here</FieldLabel>
          {acceptsServices ? null : (
            <FieldDescription>Nothing new starts here. What runs here stays until you drain.</FieldDescription>
          )}
        </FieldContent>
        {/* Turning it on mid-drain would leave the rest of the Drain nothing to move: it waits for the Drain. */}
        <Switch id={switchId} checked={acceptsServices} disabled={busy} onCheckedChange={(checked) => onAcceptsServices(checked)} />
      </Field>
      <Field orientation="responsive">
        <FieldContent>
          <FieldTitle>Drain</FieldTitle>
          <FieldDescription>{unavailable ?? <DrainLine view={view} />}</FieldDescription>
          {view.kind === "finished" && view.stoppedEarly !== null ? (
            <FieldDescription className="text-warning">It stopped early: {view.stoppedEarly}.</FieldDescription>
          ) : null}
        </FieldContent>
        <DrainButton view={view} onClick={onDrain} disabled={unavailable !== null} />
      </Field>
      {view.kind === "finished" ? <DrainResultList rows={view.rows} /> : null}
    </SettingsSection>
  );
}

/** The Drain row's words: what it does, or how the latest one went. */
function DrainLine({ view }: { view: DrainView }) {
  switch (view.kind) {
    case "idle":
      return <>Move everything running here to your other servers.</>;
    case "starting":
      return <>Starting…</>;
    case "queued":
      return view.behind === null ? <>Waiting to start…</> : <>Waits for the drain on {view.behind} to finish</>;
    case "running":
      return <>Draining · one service at a time, started <RelativeTime date={new Date(view.since)} /></>;
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
