import { type ReactNode, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { Alert, AlertDescription, AlertTitle } from "#/components/ui/alert";
import { Button } from "#/components/ui/button";
import { Spinner } from "#/components/ui/spinner";
import { reviewApproval } from "#/modules/approval-session/approval-prompt";
import { useRoleGate } from "#/modules/organization/members.queries";
import {
  type ActivationBlocker,
  activationGate,
  type ActivationServer,
} from "#/modules/server-upgrade/activation";
import {
  ActivationRefused,
  type ActivationOutcome,
  type ActivationStep,
  type ActivationSteps,
  type KeyOutcome,
  runActivation,
  setNewKey,
} from "#/modules/server-upgrade/activation-flow";
import {
  activateServersServerFn,
  generateRecipientServerFn,
  installRecipientServerFn,
} from "#/modules/server-upgrade/activation.functions";
import { activationKeys } from "#/modules/server-upgrade/activation.queries";
import { CopyBlock } from "./copy-block";

/** What this tab's run is doing, or what it ended with. The Servers' own reports drive everything else. */
type Run =
  | { readonly kind: "idle" }
  | { readonly kind: "running"; readonly step: ActivationStep }
  | { readonly kind: "activated"; readonly outcome: ActivationOutcome }
  | { readonly kind: "keyed"; readonly key: KeyOutcome }
  | { readonly kind: "failed"; readonly message: string };

const STEPS = {
  approving: "Waiting for your approval…",
  activating: "Activating servers… Each restarts once.",
  generating: "Making the new key…",
  installing: "Installing the new key…",
} as const;

const OPERATE_REFUSED = "Only an owner or admin can turn on device enforcement.";
const CLI = "ployz device enforce";

const list = (names: readonly string[]) =>
  names.length <= 1 ? names.join("") : `${names.slice(0, -1).join(", ")} and ${names.at(-1) ?? ""}`;
const plural = (names: readonly string[], one: string, many: string) => names.length === 1 ? one : many;

/** Why the dashboard can't activate, one sentence each. */
function blockerText(blocker: ActivationBlocker) {
  switch (blocker.kind) {
    case "unanswered":
      return `${list(blocker.servers)} didn't answer.`;
    case "older":
      return `${list(blocker.servers)} ${plural(blocker.servers, "runs", "run")} a Ployz older than 0.4. \`${CLI}\` leaves ${plural(blocker.servers, "it", "them")} out.`;
    case "legacy-members":
      return `${list(blocker.servers)} ${plural(blocker.servers, "lists", "list")} servers added while enforcement was off: ${list(blocker.members)}.`;
    case "legacy-keys":
      return `Keys added while enforcement was off: ${list(blocker.keys.map((key) => key.code))}.`;
    case "partial":
      return `${list(blocker.servers)} ${plural(blocker.servers, "is", "are")} already activated.`;
    case "no-cluster-key":
      return "Your servers don't all hold the same Sealing Key.";
  }
}

/**
 * On the Servers page once a Server runs Ployz 0.4: turn device enforcement on with two passkey prompts, or why it
 * can't be from here. There's no off.
 */
export function ActivationCard({ organizationSlug, servers }: {
  organizationSlug: string;
  servers: readonly ActivationServer[] | null;
}) {
  const queryClient = useQueryClient();
  const operate = useRoleGate(organizationSlug, "servers:operate", OPERATE_REFUSED);
  const [run, setRun] = useState<Run>({ kind: "idle" });
  if (servers === null) return null;
  const gate = activationGate(servers);
  const names = Object.fromEntries(servers.map((server) => [server.id, server.name]));

  const steps: ActivationSteps = {
    review: reviewApproval,
    activate: (startingSet) => activateServersServerFn({ data: { organizationSlug, startingSet } }),
    generate: (operation) => generateRecipientServerFn({ data: { organizationSlug, operation } }),
    install: (rotation, generator) => installRecipientServerFn({ data: { organizationSlug, rotation, generator } }),
    step: (step) => setRun({ kind: "running", step }),
  };
  const settle = () => queryClient.invalidateQueries({ queryKey: activationKeys.all });

  const activate = async () => {
    if (gate.kind !== "offer") return;
    try {
      setRun({ kind: "activated", outcome: await runActivation(gate, names, steps) });
    } catch (error) {
      setRun({ kind: "failed", message: error instanceof ActivationRefused ? error.message : "Ployz Cloud didn't answer. Nothing was activated." });
    }
    await settle();
  };
  const setKey = async () => {
    if (gate.kind !== "keyless") return;
    setRun({ kind: "running", step: "generating" });
    setRun({ kind: "keyed", key: await setNewKey(gate, names, steps) });
    await settle();
  };
  const dismiss = <Button type="button" variant="link" size="sm" className="h-auto px-0" onClick={() => setRun({ kind: "idle" })}>Dismiss</Button>;

  switch (run.kind) {
    case "running":
      return (
        <Alert role="status">
          <Spinner />
          <AlertTitle>Turning on device enforcement</AlertTitle>
          <AlertDescription>{STEPS[run.step]} Keep this tab open.</AlertDescription>
        </Alert>
      );
    case "failed":
      return (
        <Alert variant="destructive">
          <AlertTitle>Device enforcement wasn't turned on</AlertTitle>
          <AlertDescription className="flex flex-col gap-2"><p>{run.message}</p>{dismiss}</AlertDescription>
        </Alert>
      );
    case "activated":
      return <Outcome outcome={run.outcome} dismiss={dismiss} />;
    case "keyed":
      return <Outcome outcome={{ servers: [], key: run.key }} dismiss={dismiss} />;
    case "idle":
      break;
  }

  switch (gate.kind) {
    case "none":
    case "activated":
      return null;
    case "activating":
      return (
        <Alert role="status">
          <Spinner />
          <AlertTitle>Activating {list(gate.servers)}</AlertTitle>
          <AlertDescription>Each server restarts once.</AlertDescription>
        </Alert>
      );
    case "keyless":
      return (
        <Alert>
          <AlertTitle>No new key yet</AlertTitle>
          <AlertDescription className="flex flex-col items-start gap-2">
            <p>Values you seal still use the old key.</p>
            <Button type="button" size="sm" variant="outline" disabled={!operate.allowed} title={operate.reason} onClick={() => void setKey()}>
              Set new key
            </Button>
          </AlertDescription>
        </Alert>
      );
    case "offer":
      return (
        <Alert>
          <AlertTitle>Turn on device enforcement</AlertTitle>
          <AlertDescription className="flex flex-col items-start gap-2">
            <p>Servers then take changes only from devices and passkeys you approve. It can't be turned off.</p>
            <Button type="button" size="sm" variant="outline" disabled={!operate.allowed} title={operate.reason} onClick={() => void activate()}>
              Activate
            </Button>
          </AlertDescription>
        </Alert>
      );
    case "blocked":
      return (
        <Alert>
          <AlertTitle>Turn on device enforcement from your computer</AlertTitle>
          <AlertDescription className="flex min-w-0 flex-col gap-2">
            {gate.blockers.map((blocker) => <p key={blocker.kind}>{blockerText(blocker)}</p>)}
            <CopyBlock value={CLI} />
          </AlertDescription>
        </Alert>
      );
  }
}

/** What each Server said: activated or why not, the Private Access router it kept, and where the new key landed. */
function Outcome({ outcome, dismiss }: { outcome: ActivationOutcome; dismiss: ReactNode }) {
  const activated = outcome.servers.filter((server) => server.kind === "activated");
  const installed = outcome.key?.kind === "installed" ? outcome.key.installs.filter((server) => server.kind === "installed") : [];
  const failed = outcome.servers.length > 0 && activated.length === 0;
  const recovery = installed[0]?.name ?? null;
  const title = outcome.servers.length > 0
    ? failed ? "No server was activated" : "Device enforcement is on"
    : installed.length > 0 ? "Your servers hold the new key" : "No new key was set";
  return (
    <Alert variant={failed ? "destructive" : "default"} role="status">
      <AlertTitle>{title}</AlertTitle>
      <AlertDescription className="flex min-w-0 flex-col gap-1">
        {outcome.servers.map((server) => (
          <p key={server.id}>
            {server.kind === "activated"
              ? <>{server.name} is activated.{server.privateAccess === null ? null : <> It kept its Private Access router{server.privateAccess.tailnet === null ? "" : <> on <span className="font-mono">{server.privateAccess.tailnet}</span></>}.</>}</>
              : <>{server.name} wasn't activated. {server.why}</>}
          </p>
        ))}
        {outcome.key === null ? null : outcome.key.kind === "unset"
          ? <p>{outcome.key.why}</p>
          : outcome.key.installs.map((server) => (
            <p key={`key-${server.id}`}>
              {server.kind === "installed" ? `${server.name} holds the new key.` : `${server.name} doesn't hold the new key. ${server.why}`}
            </p>
          ))}
        {recovery === null ? null : (
          <>
            <p>No recovery code shows here. Back up the new key from your computer, with SERVER the address of {recovery}:</p>
            <CopyBlock value="ployz recovery rotate --connect ssh://root@SERVER" />
          </>
        )}
        {dismiss}
      </AlertDescription>
    </Alert>
  );
}
