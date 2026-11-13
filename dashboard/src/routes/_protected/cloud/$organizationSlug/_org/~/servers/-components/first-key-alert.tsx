import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { KeyRoundIcon } from "lucide-react";
import { Alert, AlertDescription, AlertTitle } from "#/components/ui/alert";
import { listNames } from "#/lib/plural";
import { passkeysQueryOptions, serversWithoutPasskeyQueryOptions } from "#/modules/passkeys/passkeys.queries";
import { runsOlderPloyz } from "#/modules/runtime/daemon-compat";
import { useActivation } from "#/modules/server-upgrade/activation.queries";
import { type EnforceTarget, enforceCommand } from "#/modules/server-upgrade/enforce-command";
import { CopyBlock } from "./copy-block";

type Shown = { readonly id: string; readonly name: string; readonly capabilities: readonly string[] };

/** The commands that make a Server trust passkey `code`, run on the Server itself. */
export const firstKeyCommands = (code: string | null) => `sudo ployz login\nsudo ployz device approve --passkey ${code ?? "<code>"}`;

/**
 * Which of `servers` trust no passkey yet, as a Cluster founded on Ployz 0.4 does until a person approves its first key
 * on a Server. Ployz Cloud changes nothing there until then, and can't approve the key itself. A Server on Ployz 0.3
 * isn't named: it takes only an upgrade. One awaiting its Cluster's activation deploys nothing until then either, so
 * there the passkey is only what activating from the dashboard needs, and one activating needs nothing more.
 */
export function FirstKeyAlert({ organizationSlug, servers, reach }: {
  organizationSlug: string;
  servers: readonly Shown[];
  /** Every Server of the Cluster, which activating from a computer reaches over its own SSH. */
  reach: readonly EnforceTarget[];
}) {
  const without = useQuery(serversWithoutPasskeyQueryOptions(organizationSlug));
  const own = useQuery(passkeysQueryOptions(organizationSlug));
  const lacking = servers.filter((server) => without.data?.includes(server.id) === true && !runsOlderPloyz(server));
  const activation = useActivation(organizationSlug, lacking.length > 0);
  if (activation.isPending) return null;
  const stateOf = (id: string) => activation.data?.find((server) => server.id === id)?.activation?.state ?? null;
  const waiting = lacking.filter((server) => stateOf(server.id) !== "activating");
  if (waiting.length === 0) return null;
  const awaiting = waiting.every((server) => stateOf(server.id) === "awaiting");
  const code = own.data?.passkeys[0]?.code ?? null;
  const names = waiting.map((server) => server.name);
  return (
    <Alert>
      <KeyRoundIcon />
      <AlertTitle>
        {awaiting
          ? `To activate from the dashboard, approve a passkey on ${names.length === 1 ? names[0] : listNames(names)}`
          : names.length === 1 ? `${names[0]} doesn’t trust a passkey yet` : `${listNames(names)} don’t trust a passkey yet`}
      </AlertTitle>
      <AlertDescription className="flex flex-col gap-2">
        <p>
          {awaiting ? null : "Ployz Cloud can’t deploy there until you approve one. "}
          {code === null ? (
            <>Add a passkey under{" "}
              <Link to="/cloud/$organizationSlug/~/account" params={{ organizationSlug }} hash="settings-passkeys">Account</Link>, then run this on the server:</>
          ) : names.length === 1 ? "Run this on it:" : "Run this on each:"}
        </p>
        <CopyBlock value={firstKeyCommands(code)} />
        {awaiting ? (
          <>
            <p>Or activate from an approved device, reaching each server over its own SSH:</p>
            <CopyBlock value={enforceCommand(reach)} />
          </>
        ) : null}
      </AlertDescription>
    </Alert>
  );
}
