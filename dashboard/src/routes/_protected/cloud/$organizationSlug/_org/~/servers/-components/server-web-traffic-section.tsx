import { Button } from "#/components/ui/button";
import { Spinner } from "#/components/ui/spinner";
import { Switch } from "#/components/ui/switch";
import { SettingsRow, SettingsSection } from "#/components/settings/settings-section";
import { webTrafficBusy, webTrafficChecked, type WebTrafficView } from "#/modules/machines/server-web-traffic-view";

/**
 * Whether this Server takes web traffic. On starts the web server here, so your domains point at it; off asks first
 * (the dialog is the page's), then stops it. A change under way dims the switch where it sits until the Server shows
 * the outcome; one that didn't take says why above the switch, with Retry.
 */
export function ServerWebTrafficSection({ view, onTurnOn, onAskOff, unavailable }: {
  view: WebTrafficView;
  onTurnOn: () => void;
  onAskOff: () => void;
  /** Why the switch can't move now (the Server is offline, or the page can't see it); null when it can. */
  unavailable: string | null;
}) {
  const busy = webTrafficBusy(view);
  return (
    <SettingsSection id="server-web-traffic" title="Web traffic">
      {view.kind === "failed" ? (
        <SettingsRow
          label={<>Couldn’t {view.want === "on" ? "start" : "stop"} web traffic</>}
          hint={<span role="alert" className="text-destructive">{view.reason}</span>}
          commit="none"
          control={<Button variant="outline" disabled={unavailable !== null} onClick={view.want === "on" ? onTurnOn : onAskOff}>Retry</Button>}
        />
      ) : null}
      <SettingsRow
        label="Takes web traffic"
        hint={unavailable ?? <WebTrafficLine view={view} />}
        commit="change"
        control={(
          <Switch
            aria-label="Takes web traffic"
            checked={webTrafficChecked(view)}
            disabled={busy || unavailable !== null}
            onCheckedChange={(checked) => (checked ? onTurnOn() : onAskOff())}
          />
        )}
      />
    </SettingsSection>
  );
}

/** The line under the switch: where your domains point, or what's happening. */
function WebTrafficLine({ view }: { view: WebTrafficView }) {
  switch (view.kind) {
    case "on":
      return view.address === null ? <>No public IP yet, so your domains don’t point here.</> : <>Your domains point at {view.address}.</>;
    case "starting":
      return <><Spinner className="inline-block size-3 align-[-2px]" /> Starting the web server…</>;
    case "stopping":
      return <><Spinner className="inline-block size-3 align-[-2px]" /> Stopping…</>;
    case "off":
      return <>Web traffic is off.</>;
    case "failed":
      return view.on && view.address !== null ? <>Your domains point at {view.address}.</> : <>Your domains don’t point here.</>;
  }
}
