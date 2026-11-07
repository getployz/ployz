import { startRegistration } from "@simplewebauthn/browser";
import { useMutation, useQueryClient, useSuspenseQuery } from "@tanstack/react-query";
import { getRouteApi } from "@tanstack/react-router";
import { KeyRoundIcon } from "lucide-react";
import { useState } from "react";
import { SettingsList, SettingsListItem } from "#/components/settings/settings-list";
import { SettingsSection } from "#/components/settings/settings-section";
import { Alert, AlertDescription, AlertTitle } from "#/components/ui/alert";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { beginPasskeyServerFn, finishPasskeyServerFn } from "#/modules/passkeys/passkeys.functions";
import { passkeysQueryOptions, type PasskeyRow, type PasskeysView, type ServerTrust, trustedEverywhere } from "#/modules/passkeys/passkeys.queries";
import { RelativeTime } from "#/components/relative-time";
import { CopyBlock } from "../~/servers/-components/copy-block";

/** On a server the command needs root for the local socket; a computer with an approved device runs it as the user. */
export const approveCommand = (code: string, on: "server" | "device") =>
  `${on === "server" ? "sudo " : ""}ployz device approve --passkey ${code}`;

const organizationRoute = getRouteApi("/_protected/cloud/$organizationSlug");

/**
 * Passkeys that let you approve changes from this browser. Adding one here only files it: a Server trusts it once
 * someone runs the approve command on it, and each row says what a Server reports.
 */
export function PasskeysSection() {
  const { organizationSlug } = organizationRoute.useParams();
  const { access } = organizationRoute.useLoaderData();
  return access === "member" ? <MemberPasskeys organizationSlug={organizationSlug} /> : null;
}

function MemberPasskeys({ organizationSlug }: { organizationSlug: string }) {
  const queryClient = useQueryClient();
  const { data } = useSuspenseQuery(passkeysQueryOptions(organizationSlug));
  const [failure, setFailure] = useState<string | null>(null);
  const add = useMutation({
    mutationFn: async () => {
      const options = await beginPasskeyServerFn({ data: { organizationSlug } });
      const response = await startRegistration({ optionsJSON: options });
      return finishPasskeyServerFn({ data: { organizationSlug, response: JSON.stringify(response) } });
    },
    onMutate: () => setFailure(null),
    onError: (error) => setFailure(error instanceof Error && error.name === "PasskeyRefused" ? error.message : "browser"),
    onSettled: () => queryClient.invalidateQueries(passkeysQueryOptions(organizationSlug)),
  });
  return (
    <SettingsSection id="passkeys" title="Passkeys"
      description="A passkey lets you approve changes from this browser. A server trusts it only after you approve it there."
      action={(
        <Button variant="outline" size="sm" disabled={add.isPending} onClick={() => add.mutate()}>
          Add passkey
        </Button>
      )}>
      {failure === null ? null : <AddFailed message={failure} />}
      <PasskeyList view={data} />
    </SettingsSection>
  );
}

function AddFailed({ message }: { message: string }) {
  if (message !== "browser") {
    return <Alert variant="destructive"><AlertTitle>{message}</AlertTitle></Alert>;
  }
  return (
    <Alert variant="destructive">
      <AlertTitle>This browser couldn't add a passkey.</AlertTitle>
      <AlertDescription>
        Try a browser or device with a passkey, or approve changes from a terminal with an approved device instead:
        <code className="font-mono">ployz device request</code>
      </AlertDescription>
    </Alert>
  );
}

export function PasskeyList({ view }: { view: PasskeysView }) {
  if (view.passkeys.length === 0) return null;
  return (
    <SettingsList label="Passkeys">
      {view.passkeys.map((passkey) => (
        <SettingsListItem
          key={passkey.code}
          media={<KeyRoundIcon />}
          label={<span className="font-mono">{passkey.code}</span>}
          hint={<>{passkey.account} · Added <RelativeTime date={new Date(passkey.createdAt)} /></>}
          commit="none"
          actions={<TrustBadge passkey={passkey} />}
        >
          {passkey.servers.length > 1 ? <ServerTrustList servers={passkey.servers} /> : null}
          {trustedEverywhere(passkey) ? null : <WaitingSteps code={passkey.code} />}
        </SettingsListItem>
      ))}
    </SettingsList>
  );
}

/** How many Servers trust the passkey. It approves on a Server only once that Server lists it. */
function TrustBadge({ passkey }: { passkey: PasskeyRow }) {
  const answered = passkey.servers.filter((server) => server.approval !== "unanswered");
  const trusting = answered.filter((server) => server.approval !== null);
  if (answered.length === 0) return <Badge variant="warning">No server answered</Badge>;
  if (trustedEverywhere(passkey)) {
    return <Badge variant="success">{passkey.servers.length === 1 ? `Trusted on ${passkey.servers[0]?.server}` : "Trusted on every server"}</Badge>;
  }
  if (trusting.length === 0) return <Badge variant="outline">Not yet approved</Badge>;
  return <Badge variant="warning">Trusted on {trusting.length} of {passkey.servers.length} servers</Badge>;
}

const trustText = ({ approval }: ServerTrust) =>
  approval === "unanswered" ? "Didn't answer"
  : approval === null ? "Not yet approved"
  : approval.by === "device" ? `Approved by device ${approval.code}`
  : approval.by === "server" ? "Approved on the server"
  : "Trusted";

function ServerTrustList({ servers }: { servers: readonly ServerTrust[] }) {
  return (
    <ul aria-label="Servers" className="flex flex-col gap-1 pt-2 text-sm">
      {servers.map((server) => (
        <li key={server.server} className="flex justify-between gap-4">
          <span>{server.server}</span>
          <span className="text-muted-foreground">{trustText(server)}</span>
        </li>
      ))}
    </ul>
  );
}

function WaitingSteps({ code }: { code: string }) {
  return (
    <div className="flex flex-col gap-2 pt-2">
      <p className="text-sm text-muted-foreground">On each server that doesn't trust it yet, sign in to Ployz Cloud, then approve it:</p>
      <CopyBlock value="sudo ployz login" />
      <CopyBlock value={approveCommand(code, "server")} />
      <p className="text-sm text-muted-foreground">Or run this once on a computer with an approved device:</p>
      <CopyBlock value={approveCommand(code, "device")} />
    </div>
  );
}
