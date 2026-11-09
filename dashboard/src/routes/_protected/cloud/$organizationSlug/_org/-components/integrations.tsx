import { type ReactNode, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { getRouteApi, Link } from "@tanstack/react-router";
import { ExternalLinkIcon } from "lucide-react";
import { ConfirmDialog } from "#/components/confirm-dialog";
import { CopyButton } from "#/components/copy-button";
import { SettingsRow, SettingsSection } from "#/components/settings/settings-section";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Input } from "#/components/ui/input";
import { Spinner } from "#/components/ui/spinner";
import { toErrorMessage } from "#/lib/error-message";
import { useRoleGate } from "#/modules/organization/members.queries";
import {
  authKeyProblem,
  type ObservedServer,
  type PrivateAccessChanged,
  type PrivateAccessWant,
  TAILSCALE_MACHINES_URL,
} from "#/modules/private-access/private-access";
import { setPrivateAccessServerFn } from "#/modules/private-access/private-access.functions";
import { privateAccessKeys, privateAccessQueryOptions } from "#/modules/private-access/private-access.queries";

const organizationRoute = getRouteApi("/_protected/cloud/$organizationSlug");

const LINUX = "tailscale up --accept-routes";

export function Integrations({ organizationSlug }: { organizationSlug: string }) {
  return (
    <div className="flex flex-col gap-6">
      <TailscaleSection organizationSlug={organizationSlug} />
      <SettingsSection id="cloudflare" title="Cloudflare">
        <SettingsRow label="Cloudflare Tunnel" hint="Serve web traffic through Cloudflare." commit="none"
          control={<Button variant="outline" size="sm" nativeButton={false} render={<Link to="/cloud/$organizationSlug/~/settings" params={{ organizationSlug }} search={{ section: "network" }} />}>Open Network</Button>} />
      </SettingsSection>
      <SettingsSection id="github" title="GitHub">
        <SettingsRow label="GitHub" hint="Deploy from your repositories." commit="none"
          control={<Button variant="outline" size="sm" nativeButton={false} render={<Link to="/cloud/$organizationSlug/~/settings" params={{ organizationSlug }} search={{ section: "github" }} />}>Open GitHub</Button>} />
      </SettingsSection>
    </div>
  );
}

function TailscaleSection({ organizationSlug }: { organizationSlug: string }) {
  const queryClient = useQueryClient();
  const { access } = organizationRoute.useLoaderData();
  const gate = useRoleGate(organizationSlug, "servers:operate");
  const editable = access !== "support" && gate.allowed;
  const query = useQuery(privateAccessQueryOptions(organizationSlug));
  const observed = query.data ?? null;
  const [connecting, setConnecting] = useState(false);
  const [starting, setStarting] = useState(false);
  const [turningOff, setTurningOff] = useState(false);
  const [last, setLast] = useState<Answer | null>(null);
  const [offRefusal, setOffRefusal] = useState<string | null>(null);
  const rows = observed?.servers.map((server) => ({ ...server, state: stateOf(server, last) })) ?? [];
  const placed = rows.filter((row) => row.state !== "none");
  const serving = placed.filter((row) => row.state === "running").length;
  // Until Connect answers, a running router may still be refused.
  const on = placed.length > 0 && !starting;
  const settle = async (answer: Answer) => {
    setLast(answer);
    await queryClient.invalidateQueries({ queryKey: privateAccessKeys.all });
  };

  return (
    <SettingsSection id="tailscale" title="Tailscale">
      <SettingsRow
        label="Private access"
        hint={hintOf(query.isPending, observed === null ? null : { serving, placed: on ? placed.length : 0, partial: observed.partial })}
        commit="none"
        control={(
          <>
            {on ? <StatusBadge serving={serving} placed={placed.length} /> : null}
            {editable && on ? <Button type="button" variant="outline" size="sm" onClick={() => setTurningOff(true)}>Turn off</Button> : null}
            {editable && on && serving < placed.length && !connecting ? <Button type="button" size="sm" onClick={() => setConnecting(true)}>Try another key</Button> : null}
            {editable && placed.length === 0 && !connecting && observed !== null ? <Button type="button" size="sm" onClick={() => setConnecting(true)}>Connect</Button> : null}
          </>
        )}
      >
        {offRefusal === null ? null : <p role="alert" className="text-sm text-destructive">{offRefusal}</p>}
        {last === null || last.changed.failures.length === 0 ? null : (
          <ul role="alert" aria-label="Failures" className="flex flex-col gap-1 text-sm text-destructive">
            {last.changed.failures.map((failure) => <li key={failure.id}>{failure.message}</li>)}
          </ul>
        )}
      </SettingsRow>
      {connecting ? (
        <ConnectForm
          organizationSlug={organizationSlug}
          onCancel={() => setConnecting(false)}
          onPending={setStarting}
          onDone={async (changed) => {
            if (changed.failures.length === 0) setConnecting(false);
            await settle({ to: "on", changed, at: Date.now() });
          }}
        />
      ) : null}
      {on ? <Routers rows={rows} /> : null}
      {on && serving > 0 ? (
        <>
          <SettingsRow label="Approve the route" hint={<>Once per server, unless your tailnet auto-approves <span className="font-mono">tag:ployz</span>.</>} commit="none"
            control={<Button variant="outline" size="sm" nativeButton={false} render={<a href={TAILSCALE_MACHINES_URL} target="_blank" rel="noreferrer" />}>Open Tailscale<ExternalLinkIcon aria-hidden /></Button>} />
          <SettingsRow label="Linux" hint={<span className="font-mono break-all">{LINUX}</span>} commit="none"
            control={<CopyButton value={LINUX} label="Copy command" size="icon-xs" />} />
        </>
      ) : null}
      <ConfirmDialog
        open={turningOff}
        onOpenChange={setTurningOff}
        title="Turn off private access?"
        description={`Removes ${placed.length} ${placed.length === 1 ? "router" : "routers"} from your tailnet. Turning it on again needs new route approvals.`}
        actionLabel="Turn off"
        pendingLabel="Turning off…"
        variant="destructive"
        onConfirm={async () => {
          setOffRefusal(null);
          try {
            await settle({ to: "off", changed: await setPrivateAccessServerFn({ data: { organizationSlug, to: "off" } }), at: Date.now() });
          } catch (error) {
            setOffRefusal(toErrorMessage(error, "Your servers didn't answer. Try again."));
          }
          setTurningOff(false);
        }}
      />
    </SettingsSection>
  );
}

type Answer = { readonly to: PrivateAccessWant; readonly changed: PrivateAccessChanged; readonly at: number };

/** How long a removal outranks the Servers' watch, which lists a removed router for a few seconds. */
const REMOVAL_LAG_MS = 30_000;

type RowState = ObservedServer["router"] | "refused";

/**
 * The last answer outranks the poll: a refused router runs until Tailscale gives up, and a
 * removed one can still be listed until the Servers' watch catches up.
 */
function stateOf(server: ObservedServer, last: Answer | null): RowState {
  if (server.router === "none" || last === null) return server.router;
  const failed = last.changed.failures.some((failure) => failure.id === server.id);
  if (failed) return last.to === "on" ? "refused" : server.router;
  const removed = last.to === "off" && Date.now() - last.at < REMOVAL_LAG_MS && last.changed.servers.some((row) => row.id === server.id && !row.router);
  return removed ? "none" : server.router;
}

function hintOf(loading: boolean, routers: { serving: number; placed: number; partial: boolean } | null) {
  if (loading) return "Checking your servers…";
  if (routers === null) return "Your servers aren't answering.";
  if (routers.placed === 0) return "Reach your servers from your tailnet.";
  if (routers.serving === 0) return "No router is running. Try another key, or turn it off.";
  const shown = `${routers.serving} ${routers.serving === 1 ? "router" : "routers"}`;
  return routers.partial ? `${shown} · some servers didn't answer` : shown;
}

function StatusBadge({ serving, placed }: { serving: number; placed: number }) {
  if (serving === 0) return <Badge variant="warning">Not connected</Badge>;
  if (serving < placed) return <Badge variant="warning">Partly connected</Badge>;
  return <Badge variant="success">Connected</Badge>;
}

const ROW_BADGE = {
  running: <Badge variant="success">Running</Badge>,
  refused: <Badge variant="destructive">Key refused</Badge>,
  stopped: <Badge variant="warning">Stopped</Badge>,
  none: <Badge variant="warning">No router</Badge>,
} satisfies Record<RowState, ReactNode>;

function Routers({ rows }: { rows: readonly (ObservedServer & { state: RowState })[] }) {
  return (
    <SettingsRow label="Routers" commit="none">
      <ul aria-label="Routers" className="flex flex-col gap-1 text-sm">
        {rows.map((row) => (
          <li key={row.id} className="flex items-center gap-2">
            <span className="min-w-0 flex-1 truncate">{row.name}</span>
            {ROW_BADGE[row.state]}
          </li>
        ))}
      </ul>
    </SettingsRow>
  );
}

function ConnectForm({ organizationSlug, onCancel, onPending, onDone }: {
  organizationSlug: string;
  onCancel: () => void;
  onPending: (pending: boolean) => void;
  onDone: (changed: PrivateAccessChanged) => Promise<void>;
}) {
  const [key, setKey] = useState("");
  const [busy, setBusy] = useState(false);
  const [refusal, setRefusal] = useState<string | null>(null);
  const problem = key === "" ? null : authKeyProblem(key);
  const connect = async () => {
    setBusy(true);
    onPending(true);
    setRefusal(null);
    try {
      const changed = await setPrivateAccessServerFn({ data: { organizationSlug, to: "on", authKey: key.trim() } });
      setKey("");
      await onDone(changed);
    } catch (error) {
      setRefusal(toErrorMessage(error, "Your servers didn't answer. Try again."));
    } finally {
      setBusy(false);
      onPending(false);
    }
  };
  return (
    <form
      onSubmit={(event) => {
        event.preventDefault();
        if (!busy && key !== "" && problem === null) void connect();
      }}
      onKeyDown={(event) => {
        if (event.key === "Escape") onCancel();
      }}
    >
      <SettingsRow
        label="Auth key"
        hint={<>Use a reusable key tagged <span className="font-mono">tag:ployz</span>.</>}
        commit="form"
        control={(
          <div className="flex w-full gap-2">
            <Input aria-label="Auth key" type="password" value={key} autoFocus spellCheck={false} autoComplete="off" disabled={busy}
              aria-invalid={problem !== null} aria-describedby={problem === null ? undefined : "auth-key-problem"} className="font-mono"
              onChange={(event) => setKey(event.target.value)} />
            <Button type="submit" size="sm" disabled={busy || key === "" || problem !== null}>{busy ? <Spinner /> : null}Connect</Button>
            <Button type="button" size="sm" variant="ghost" disabled={busy} onClick={onCancel}>Cancel</Button>
          </div>
        )}
      >
        {problem === null ? null : <p id="auth-key-problem" className="text-sm text-destructive">{problem}</p>}
        {busy ? <p role="status" className="text-sm text-muted-foreground">Connecting servers…</p> : null}
        {refusal === null ? null : <p role="alert" className="text-sm text-destructive">{refusal}</p>}
      </SettingsRow>
    </form>
  );
}
