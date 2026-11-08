import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { getRouteApi } from "@tanstack/react-router";
import { toast } from "sonner";
import { ConfirmDialog } from "#/components/confirm-dialog";
import { SettingsRow, SettingsSection } from "#/components/settings/settings-section";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Input } from "#/components/ui/input";
import { Item, ItemContent, ItemGroup, ItemTitle } from "#/components/ui/item";
import { Spinner } from "#/components/ui/spinner";
import { toErrorMessage } from "#/lib/error-message";
import { normalizeZone, zoneProblem } from "#/modules/cloudflare/zone";
import {
  type CloudflareSetup,
  type ClusterIngressObservation,
  type DoorsWant,
  doorWords,
  holdingCount,
  type LatestDoorsChange,
  switchProblem,
  switchProgress,
} from "#/modules/ingress/doors";
import {
  listCloudflareZonesServerFn,
  reviewDoorsSwitchServerFn,
  setUpCloudflareServerFn,
  switchDoorsServerFn,
} from "#/modules/ingress/doors.functions";
import { clusterIngressKeys, clusterIngressObservationQueryOptions, useClusterIngress } from "#/modules/ingress/doors.queries";
import { useRoleGate } from "#/modules/organization/members.queries";

const organizationRoute = getRouteApi("/_protected/cloud/$organizationSlug");

/** Web traffic: whether the Cluster takes it on public ports or through a Cloudflare tunnel, and the switch between them. */
export function WebTrafficSection({ organizationSlug }: { organizationSlug: string }) {
  const queryClient = useQueryClient();
  const view = useClusterIngress(organizationSlug);
  const { access } = organizationRoute.useLoaderData();
  const gate = useRoleGate(organizationSlug, "servers:operate");
  const editable = access !== "support" && gate.allowed;
  const { setup, change } = view;
  const switching = change !== null && (change.state === "pending" || change.state === "running");
  const failure = change !== null && change.state === "failed" ? change.failure : null;
  const observed = useQuery({
    ...clusterIngressObservationQueryOptions(organizationSlug, switching || failure !== null || setup?.state === "active"),
    enabled: setup !== null,
  }).data ?? null;
  const [settingUp, setSettingUp] = useState(false);
  const [reviewing, setReviewing] = useState<DoorsWant | null>(null);
  const progress = switchProgress(change);
  const refresh = () => queryClient.invalidateQueries({ queryKey: clusterIngressKeys.all });
  const onCloudflare = setup?.state === "active";
  const canSwitchBack = setup !== null && switchProblem(setup, "public", change) === null && (onCloudflare || change?.state === "failed");

  return (
    <SettingsSection id="web-traffic" title="Web traffic">
      <SettingsRow
        label="Public ports"
        hint={publicHint(setup, observed)}
        commit="none"
        control={canSwitchBack ? (
          editable ? <Button type="button" variant="outline" size="sm" disabled={switching} onClick={() => setReviewing("public")}>Switch back</Button> : null
        ) : onCloudflare || switching ? null : <Badge variant="info">In use</Badge>}
      />
      <SettingsRow
        label="Cloudflare Tunnel"
        hint={cloudflareHint(setup, observed, switching)}
        commit="none"
        control={cloudflareControl({ setup, change, switching, editable, settingUp, onSetUp: () => setSettingUp(true), onSwitch: () => setReviewing("tunnel") })}
      >
        {failure === null ? null : <p role="alert" className="text-sm text-destructive">{failure}</p>}
        {progress === null ? null : (
          <p role="status" className="flex items-center gap-2 text-sm text-muted-foreground"><Spinner />{progress.label}. Step {progress.step} of 3.</p>
        )}
      </SettingsRow>
      {settingUp && (setup === null || setup.state === "left") ? (
        <CloudflareSetupForm
          organizationSlug={organizationSlug}
          onCancel={() => setSettingUp(false)}
          onDone={() => {
            setSettingUp(false);
            void refresh();
          }}
        />
      ) : null}
      {reviewing === null ? null : (
        <SwitchDialog
          organizationSlug={organizationSlug}
          to={reviewing}
          onOpenChange={(open) => {
            if (!open) setReviewing(null);
          }}
          onConfirm={async () => {
            try {
              await switchDoorsServerFn({ data: { organizationSlug, to: reviewing } });
              await refresh();
              setReviewing(null);
            } catch (error) {
              toast.error(toErrorMessage(error, "Could not switch web traffic"));
            }
          }}
        />
      )}
    </SettingsSection>
  );
}

/** What the Servers show of their ports, once seen; before that, what the source says they should show. */
function publicHint(setup: CloudflareSetup | null, observed: ClusterIngressObservation | null) {
  if (setup?.state === "left") return `Delete the tunnel ${setup.tunnelName} and *.${setup.zone} in Cloudflare.`;
  if (setup === null) return "Ports 80 and 443 open on every server.";
  return doorWords(observed, "public") ?? (setup.state === "active" ? "Ports 80 and 443 closed." : "Ports 80 and 443 open on every server.");
}

/** What the Servers show of the tunnel, once seen, so a switch under way reads as the Doors stand, not as it will end. */
function cloudflareHint(setup: CloudflareSetup | null, observed: ClusterIngressObservation | null, switching: boolean) {
  if (setup === null || setup.state === "left") return "Serve through Cloudflare with no open ports.";
  if (setup.state === "set_up" && !switching) return `Tunnel ready on ${setup.zone}`;
  if (observed === null) return setup.zone;
  return `${setup.zone} · ${holdingCount(observed.servers, "tunnel")} of ${observed.servers.length} servers connected`;
}

function cloudflareControl({ setup, change, switching, editable, settingUp, onSetUp, onSwitch }: {
  setup: CloudflareSetup | null;
  change: LatestDoorsChange | null;
  switching: boolean;
  editable: boolean;
  settingUp: boolean;
  onSetUp: () => void;
  onSwitch: () => void;
}) {
  const canSwitch = setup !== null && switchProblem(setup, "tunnel", change) === null;
  if (setup?.state === "active" && !canSwitch) return switching ? null : <Badge variant="info">In use</Badge>;
  if (!editable) return null;
  if (setup === null || setup.state === "left") {
    return <Button type="button" variant="outline" size="sm" disabled={settingUp} onClick={onSetUp}>Set up</Button>;
  }
  return <Button type="button" size="sm" disabled={switching} onClick={onSwitch}>Review and switch</Button>;
}

function CloudflareSetupForm({ organizationSlug, onCancel, onDone }: {
  organizationSlug: string;
  onCancel: () => void;
  onDone: () => void;
}) {
  const [token, setToken] = useState("");
  const [zones, setZones] = useState<readonly string[] | null>(null);
  const [zone, setZone] = useState("");
  const [busy, setBusy] = useState<"zones" | "setup" | null>(null);
  const [refusal, setRefusal] = useState<string | null>(null);
  const problem = zones === null ? null : zoneProblem(zone, zones);

  const findZones = async () => {
    setBusy("zones");
    setRefusal(null);
    try {
      const found = await listCloudflareZonesServerFn({ data: { organizationSlug, token: token.trim() } });
      setZones(found.zones);
      if (zone === "" && found.zones.length === 1) setZone(found.zones[0] ?? "");
    } catch (error) {
      setRefusal(toErrorMessage(error, "Cloudflare didn't answer. Try again."));
    } finally {
      setBusy(null);
    }
  };
  const setUp = async () => {
    setBusy("setup");
    setRefusal(null);
    try {
      await setUpCloudflareServerFn({ data: { organizationSlug, token: token.trim(), zone: normalizeZone(zone) } });
      setToken("");
      onDone();
    } catch (error) {
      setRefusal(toErrorMessage(error, `Cloudflare setup didn't finish. Check Cloudflare for a tunnel named ployz-${organizationSlug}.`));
    } finally {
      setBusy(null);
    }
  };

  return (
    <form
      onSubmit={(event) => {
        event.preventDefault();
        if (busy === null) void (zones === null ? findZones() : problem === null && zone !== "" ? setUp() : undefined);
      }}
      onKeyDown={(event) => {
        if (event.key === "Escape") onCancel();
      }}
    >
      <SettingsRow
        label="Cloudflare API token"
        hint="Needs Zone: Zone: Read, Zone: DNS: Edit and Account: Cloudflare Tunnel: Edit. Used once, never stored."
        commit="form"
        control={(
          <div className="flex w-full gap-2">
            <Input
              aria-label="Cloudflare API token"
              type="password"
              value={token}
              autoFocus
              spellCheck={false}
              autoComplete="off"
              disabled={zones !== null || busy !== null}
              className="font-mono"
              onChange={(event) => setToken(event.target.value)}
            />
            {zones === null ? <Button type="submit" size="sm" disabled={token.trim() === "" || busy !== null}>{busy === "zones" ? <Spinner /> : null}Find zones</Button> : null}
          </div>
        )}
      />
      {zones === null ? null : (
        <SettingsRow
          label="Zone"
          hint={zones.length === 0 ? "This token can't see any zones." : "Addresses go one level below it, like web-prod.example.com."}
          commit="form"
          control={(
            <div className="flex w-full gap-2">
              <Input
                aria-label="Zone"
                list="cloudflare-zones"
                value={zone}
                spellCheck={false}
                autoComplete="off"
                aria-invalid={problem !== null && zone !== ""}
                aria-describedby={problem === null ? undefined : "cloudflare-zone-problem"}
                className="font-mono"
                onChange={(event) => setZone(event.target.value)}
              />
              <datalist id="cloudflare-zones">{zones.map((name) => <option key={name} value={name} />)}</datalist>
              <Button type="submit" size="sm" disabled={problem !== null || zone === "" || busy !== null}>{busy === "setup" ? <Spinner /> : null}Set up</Button>
            </div>
          )}
        >
          {problem === null || zone === "" ? null : <p id="cloudflare-zone-problem" className="text-sm text-destructive">{problem}</p>}
        </SettingsRow>
      )}
      <SettingsRow label="" commit="none" control={<Button type="button" variant="ghost" size="sm" disabled={busy !== null} onClick={onCancel}>Cancel</Button>}>
        {refusal === null ? null : <p role="alert" className="text-sm text-destructive">{refusal}</p>}
      </SettingsRow>
    </form>
  );
}

function SwitchDialog({ organizationSlug, to, onOpenChange, onConfirm }: {
  organizationSlug: string;
  to: DoorsWant;
  onOpenChange: (open: boolean) => void;
  onConfirm: () => Promise<void>;
}) {
  const review = useQuery({
    queryKey: [...clusterIngressKeys.all, organizationSlug, "review", to] as const,
    queryFn: () => reviewDoorsSwitchServerFn({ data: { organizationSlug, to } }),
    gcTime: 0,
  });
  const moves = review.data?.moves ?? [];
  const toCloudflare = to === "tunnel";
  return (
    <ConfirmDialog
      open
      onOpenChange={onOpenChange}
      title={toCloudflare ? "Switch to Cloudflare Tunnel?" : "Switch back to public ports?"}
      description={(
        <div className="flex flex-col gap-4">
          <p>
            {toCloudflare
              ? "Servers close ports 80 and 443 and take traffic through the tunnel. Expect a short interruption."
              : "Servers open ports 80 and 443 and the tunnel closes. Expect a short interruption."}
          </p>
          {review.isPending ? <p role="status" className="flex items-center gap-2"><Spinner />Checking addresses</p> : null}
          {review.isError ? <p role="alert">Couldn't check the addresses. You can still switch.</p> : null}
          {moves.length === 0 ? null : (
            <>
              <p>These addresses change. The old ones stop working.</p>
              <ItemGroup aria-label="Addresses that move" className="max-h-72 overflow-y-auto text-foreground">
                {moves.map(({ from, to: next }) => (
                  <Item key={from} variant="outline" size="xs" data-row={from}>
                    <ItemContent className="min-w-0">
                      <ItemTitle className="font-mono break-words">{from} → {next}</ItemTitle>
                    </ItemContent>
                  </Item>
                ))}
              </ItemGroup>
            </>
          )}
        </div>
      )}
      actionLabel="Switch"
      pendingLabel="Switching"
      variant={moves.length > 0 ? "destructive" : "default"}
      onConfirm={onConfirm}
    />
  );
}
