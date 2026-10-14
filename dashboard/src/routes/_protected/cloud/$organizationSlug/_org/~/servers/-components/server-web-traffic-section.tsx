import { useId } from "react";
import { TriangleAlertIcon } from "lucide-react";
import { Alert, AlertAction, AlertDescription, AlertTitle } from "#/components/ui/alert";
import { Button } from "#/components/ui/button";
import { Field, FieldContent, FieldDescription, FieldLabel } from "#/components/ui/field";
import { Spinner } from "#/components/ui/spinner";
import { Switch } from "#/components/ui/switch";
import { webTrafficBusy, webTrafficChecked, type WebTrafficView } from "#/modules/machines/server-web-traffic-view";
import { SettingsSection } from "#/routes/_protected/cloud/$organizationSlug/-components/SettingsSection";

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
  const switchId = useId();
  const busy = webTrafficBusy(view);
  return (
    <SettingsSection id="server-web-traffic" title="Web traffic">
      {view.kind === "failed" ? (
        <Alert variant="destructive">
          <TriangleAlertIcon />
          <AlertTitle>Couldn’t {view.want === "on" ? "start" : "stop"} web traffic</AlertTitle>
          <AlertDescription>{view.reason}</AlertDescription>
          <AlertAction>
            <Button variant="outline" size="sm" disabled={unavailable !== null} onClick={view.want === "on" ? onTurnOn : onAskOff}>Retry</Button>
          </AlertAction>
        </Alert>
      ) : null}
      <Field orientation="horizontal">
        <FieldContent>
          <FieldLabel htmlFor={switchId}>Takes web traffic</FieldLabel>
          <FieldDescription>{unavailable ?? <WebTrafficLine view={view} />}</FieldDescription>
        </FieldContent>
        <Switch
          id={switchId}
          checked={webTrafficChecked(view)}
          disabled={busy || unavailable !== null}
          onCheckedChange={(checked) => (checked ? onTurnOn() : onAskOff())}
        />
      </Field>
    </SettingsSection>
  );
}

/** The line under the switch: where your domains point, or what's happening. */
function WebTrafficLine({ view }: { view: WebTrafficView }) {
  switch (view.kind) {
    case "on":
      return view.address === null ? <>Your domains point here.</> : <>Your domains point at {view.address}.</>;
    case "starting":
      return <><Spinner className="inline-block size-3 align-[-2px]" /> Starting the web server…</>;
    case "stopping":
      return <><Spinner className="inline-block size-3 align-[-2px]" /> Stopping…</>;
    case "off":
      return <>Your domains don’t point here.</>;
    case "failed":
      return view.on ? <>Your domains point at {view.address ?? "this server"}.</> : <>Your domains don’t point here.</>;
  }
}
