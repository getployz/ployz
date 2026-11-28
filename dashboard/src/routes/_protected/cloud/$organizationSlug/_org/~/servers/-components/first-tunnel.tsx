import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Alert, AlertDescription, AlertTitle } from "#/components/ui/alert";
import { Button } from "#/components/ui/button";
import { Spinner } from "#/components/ui/spinner";
import { toast } from "sonner";
import { toErrorMessage } from "#/lib/error-message";
import { openPublicPortsServerFn } from "#/modules/ingress/doors.functions";
import { clusterIngressKeys, clusterIngressObservationQueryOptions, clusterIngressQueryOptions } from "#/modules/ingress/doors.queries";
import { useRoleGate } from "#/modules/organization/members.queries";
import { CloudflareChoice } from "#/modules/machines/add-server/install-command";
import { forgetCloudflare, holdCloudflare, runFirstSetup, useHeldCloudflare } from "#/modules/ingress/cloudflare-setup";
import { type CardCloudflare } from "#/modules/machines/add-server/cloudflare-choice";
import { firstTunnelState } from "./servers-page-state";

/** On the Servers page once the first Server joined: how the Cloudflare Tunnel chosen for it stands, and the ways on. */
export function FirstTunnelStatus({ organizationSlug }: { organizationSlug: string }) {
  const queryClient = useQueryClient();
  const held = useHeldCloudflare(organizationSlug);
  const view = useQuery(clusterIngressQueryOptions(organizationSlug)).data;
  const look = view !== undefined && view.foundedClosed && view.setup === null && held === undefined;
  const observed = useQuery({ ...clusterIngressObservationQueryOptions(organizationSlug, false), enabled: look }).data ?? null;
  const state = view === undefined
    ? { kind: "none" as const }
    : firstTunnelState({ held, foundedClosed: view.foundedClosed, setup: view.setup, observed: look ? observed : null });

  switch (state.kind) {
    case "none":
    case "waiting":
      return null;
    case "setting-up":
      return (
        <Alert role="status">
          <Spinner />
          <AlertTitle>Setting up Cloudflare Tunnel on <span className="font-mono">{state.zone}</span>…</AlertTitle>
          <AlertDescription>Keep this tab open until it finishes.</AlertDescription>
        </Alert>
      );
    case "done":
      return (
        <Alert role="status">
          <AlertTitle>Cloudflare Tunnel is set up on <span className="font-mono">{state.zone}</span></AlertTitle>
          <AlertDescription className="flex flex-wrap items-center gap-x-2">
            {state.switched ? null : <span>To move apps you already deployed onto it, switch web traffic under Organization, Network.</span>}
            <Button type="button" variant="link" size="sm" className="h-auto px-0" onClick={() => forgetCloudflare(organizationSlug)}>Dismiss</Button>
          </AlertDescription>
        </Alert>
      );
    case "failed":
      return (
        <Alert variant="destructive">
          <AlertTitle>Cloudflare Tunnel setup didn't finish</AlertTitle>
          <AlertDescription className="flex flex-col gap-2">
            <p>{state.message}</p>
            <div className="flex flex-wrap gap-2">
              <Button type="button" size="sm" variant="outline" onClick={() => void runFirstSetup(organizationSlug, queryClient)}>Try again</Button>
              <OpenPublicPorts organizationSlug={organizationSlug} />
            </div>
          </AlertDescription>
        </Alert>
      );
    case "unfinished":
      return <Unfinished organizationSlug={organizationSlug} />;
  }
}

/** A Cluster founded closed with no token left in this tab, as after a reload: paste it again, or open the ports. */
function Unfinished({ organizationSlug }: { organizationSlug: string }) {
  const queryClient = useQueryClient();
  const [choice, setChoice] = useState<CardCloudflare>({ at: "token" });
  const [token, setToken] = useState("");
  return (
    <Alert>
      <AlertTitle>Your apps aren't reachable yet</AlertTitle>
      <AlertDescription className="flex min-w-0 flex-col gap-3">
        <p>Cloudflare Tunnel setup didn't finish, and ports 80 and 443 are closed.</p>
        <CloudflareChoice
          organizationSlug={organizationSlug}
          choice={choice}
          token={token}
          onToken={setToken}
          onChoice={(next) => {
            setChoice(next.at === "off" ? { at: "token" } : next);
            if (next.at !== "ready") return;
            holdCloudflare(organizationSlug, { kind: "waiting", token, zone: next.zone });
            setToken("");
            void runFirstSetup(organizationSlug, queryClient);
          }}
        />
        <OpenPublicPorts organizationSlug={organizationSlug} />
      </AlertDescription>
    </Alert>
  );
}

function OpenPublicPorts({ organizationSlug }: { organizationSlug: string }) {
  const queryClient = useQueryClient();
  const operate = useRoleGate(organizationSlug, "servers:operate");
  const [busy, setBusy] = useState(false);
  return (
    <Button
      type="button"
      size="sm"
      variant="ghost"
      className="w-fit"
      disabled={busy || !operate.allowed}
      title={operate.reason}
      onClick={async () => {
        setBusy(true);
        try {
          await openPublicPortsServerFn({ data: { organizationSlug } });
          forgetCloudflare(organizationSlug);
          toast.success("Your servers take web traffic on ports 80 and 443.");
          await queryClient.invalidateQueries({ queryKey: clusterIngressKeys.all });
        } catch (error) {
          toast.error(toErrorMessage(error, "Couldn't open ports 80 and 443."));
        } finally {
          setBusy(false);
        }
      }}
    >
      {busy ? <Spinner data-icon="inline-start" /> : null}
      Use public ports instead
    </Button>
  );
}
