import { useEffect, useState } from "react";
import { type QueryClient, useQuery, useQueryClient } from "@tanstack/react-query";
import { Alert, AlertDescription, AlertTitle } from "#/components/ui/alert";
import { Button } from "#/components/ui/button";
import { Spinner } from "#/components/ui/spinner";
import { toast } from "sonner";
import { toErrorMessage } from "#/lib/error-message";
import { openPublicPortsServerFn, setUpCloudflareServerFn } from "#/modules/ingress/doors.functions";
import { clusterIngressKeys, clusterIngressObservationQueryOptions, clusterIngressQueryOptions } from "#/modules/ingress/doors.queries";
import { serverEnrollmentsQueryOptions } from "#/modules/machines/waiting/waiting.queries";
import { useRoleGate } from "#/modules/organization/members.queries";
import { CloudflareChoice } from "./cluster-setup";
import { forgetCloudflare, holdCloudflare, readHeldCloudflare, useHeldCloudflare } from "./first-cloudflare";
import { type CardCloudflare, firstTunnelState } from "./servers-page-state";

const NOT_ANSWERING = "Your servers aren't answering";
const ANSWER_TRIES = 12;
const ANSWER_WAIT_MS = 5_000;

/**
 * Set Cloudflare up with the token this tab holds, once. A Cluster just founded may not answer for a few seconds, so
 * that refusal is asked again a while; any other ends the setup with its words, and the token stays for Try again.
 */
async function runFirstSetup(organizationSlug: string, queryClient: QueryClient) {
  const held = readHeldCloudflare(organizationSlug);
  if (held === undefined || (held.kind !== "waiting" && held.kind !== "failed")) return;
  const { token, zone } = held;
  holdCloudflare(organizationSlug, { kind: "setting-up", token, zone });
  for (let tried = 1; ; tried++) {
    try {
      const row = await setUpCloudflareServerFn({ data: { organizationSlug, token: token.trim(), zone } });
      holdCloudflare(organizationSlug, { kind: "done", zone, switched: row.state === "active" });
      toast.success(`Cloudflare Tunnel is set up on ${zone}.`);
      break;
    } catch (error) {
      const message = toErrorMessage(error, "Try again, or use public ports instead.");
      if (message.startsWith(NOT_ANSWERING) && tried < ANSWER_TRIES) {
        await new Promise((resolve) => setTimeout(resolve, ANSWER_WAIT_MS));
        continue;
      }
      holdCloudflare(organizationSlug, { kind: "failed", token, zone, message });
      break;
    }
  }
  await queryClient.invalidateQueries({ queryKey: clusterIngressKeys.all });
}

/** Wherever the user goes in the Organization, the held setup runs as soon as the first Server founds the Cluster. */
export function FirstCloudflareRunner({ organizationSlug }: { organizationSlug: string }) {
  const held = useHeldCloudflare(organizationSlug);
  return held?.kind === "waiting" ? <AwaitFounder organizationSlug={organizationSlug} /> : null;
}

function AwaitFounder({ organizationSlug }: { organizationSlug: string }) {
  const queryClient = useQueryClient();
  const founded = useQuery(serverEnrollmentsQueryOptions(organizationSlug)).data?.founded === true;
  useEffect(() => {
    if (founded) void runFirstSetup(organizationSlug, queryClient);
  }, [founded, organizationSlug, queryClient]);
  return null;
}

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
          <AlertTitle>Cloudflare Tunnel is ready</AlertTitle>
          <AlertDescription className="flex flex-wrap items-center gap-x-2">
            {state.switched
              ? <span>Apps you deploy get addresses like <span className="font-mono break-all">web-production.{state.zone}</span>.</span>
              : <span>Apps you already deployed keep their addresses. To move them to <span className="font-mono break-all">{state.zone}</span>, switch web traffic in your organization's Network settings.</span>}
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
        <p>Cloudflare Tunnel setup didn't finish, and ports 80 and 443 are closed. Paste your Cloudflare API token to finish, or use public ports.</p>
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
          toast.success("Your servers now take web traffic on ports 80 and 443.");
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
