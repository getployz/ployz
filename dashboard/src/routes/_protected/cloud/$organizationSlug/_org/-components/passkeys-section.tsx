import { startRegistration } from "@simplewebauthn/browser";
import { useMutation, useQueryClient, useSuspenseQuery } from "@tanstack/react-query";
import { useParams } from "@tanstack/react-router";
import { KeyRoundIcon } from "lucide-react";
import { useState } from "react";
import { SettingsList, SettingsListItem } from "#/components/settings/settings-list";
import { SettingsSection } from "#/components/settings/settings-section";
import { Alert, AlertDescription, AlertTitle } from "#/components/ui/alert";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { beginPasskeyServerFn, finishPasskeyServerFn } from "#/modules/passkeys/passkeys.functions";
import { passkeysQueryOptions, type PasskeyRow, type PasskeysView } from "#/modules/passkeys/passkeys.queries";
import { RelativeTime } from "#/components/relative-time";
import { CopyBlock } from "../~/servers/-components/copy-block";

export const approveCommand = (code: string) => `sudo ployz device approve --passkey ${code}`;

/**
 * Passkeys that let you approve changes from this browser. Adding one here only files it: a Server trusts it once
 * someone runs the approve command on it, and each row says what a Server reports.
 */
export function PasskeysSection() {
  const { organizationSlug } = useParams({ from: "/_protected/cloud/$organizationSlug" });
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
          actions={<ApprovalBadge passkey={passkey} observed={view.observed} />}
        >
          {passkey.approval === null ? <WaitingSteps code={passkey.code} /> : null}
        </SettingsListItem>
      ))}
    </SettingsList>
  );
}

function ApprovalBadge({ passkey, observed }: { passkey: PasskeyRow; observed: boolean }) {
  const approval = passkey.approval;
  if (approval?.by === "device") return <Badge variant="success">Approved by device {approval.code}</Badge>;
  if (approval?.by === "server") return <Badge variant="success">Approved on {approval.server}</Badge>;
  if (!observed) return <Badge variant="warning">No server answered</Badge>;
  return <Badge variant="outline">Not yet approved by a device</Badge>;
}

function WaitingSteps({ code }: { code: string }) {
  return (
    <div className="flex flex-col gap-2 pt-2">
      <p className="text-sm text-muted-foreground">
        Run this on one of your servers, or on a computer with an approved device, and check the code matches:
      </p>
      <CopyBlock value={approveCommand(code)} />
    </div>
  );
}
