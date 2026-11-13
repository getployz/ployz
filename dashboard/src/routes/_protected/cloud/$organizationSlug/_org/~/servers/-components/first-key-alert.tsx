import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { KeyRoundIcon } from "lucide-react";
import { Alert, AlertDescription, AlertTitle } from "#/components/ui/alert";
import { listNames } from "#/lib/plural";
import { passkeysQueryOptions, serversWithoutPasskeyQueryOptions } from "#/modules/passkeys/passkeys.queries";
import { runsOlderPloyz } from "#/modules/runtime/daemon-compat";
import { CopyBlock } from "./copy-block";

type Shown = { readonly id: string; readonly name: string; readonly capabilities: readonly string[] };

/** The commands that make a Server trust passkey `code`, run on the Server itself. */
export const firstKeyCommands = (code: string | null) => `sudo ployz login\nsudo ployz device approve --passkey ${code ?? "<code>"}`;

/**
 * Which of `servers` trust no passkey yet, as a Cluster founded on Ployz 0.4 does until a person approves its first key
 * on a Server. Ployz Cloud changes nothing there until then, and can't approve the key itself. A Server on Ployz 0.3
 * isn't named: it takes only an upgrade.
 */
export function FirstKeyAlert({ organizationSlug, servers }: { organizationSlug: string; servers: readonly Shown[] }) {
  const without = useQuery(serversWithoutPasskeyQueryOptions(organizationSlug));
  const own = useQuery(passkeysQueryOptions(organizationSlug));
  const waiting = servers.filter((server) => without.data?.includes(server.id) === true && !runsOlderPloyz(server));
  if (waiting.length === 0) return null;
  const code = own.data?.passkeys[0]?.code ?? null;
  const names = waiting.map((server) => server.name);
  return (
    <Alert>
      <KeyRoundIcon />
      <AlertTitle>{names.length === 1 ? `${names[0]} doesn’t trust a passkey yet` : `${listNames(names)} don’t trust a passkey yet`}</AlertTitle>
      <AlertDescription className="flex flex-col gap-2">
        <p>
          Ployz Cloud can’t deploy there until you approve one.{" "}
          {code === null ? (
            <>Add a passkey under{" "}
              <Link to="/cloud/$organizationSlug/~/account" params={{ organizationSlug }} hash="passkeys">Account</Link>, then run this on the server:</>
          ) : names.length === 1 ? "Run this on it:" : "Run this on each:"}
        </p>
        <CopyBlock value={firstKeyCommands(code)} />
      </AlertDescription>
    </Alert>
  );
}
