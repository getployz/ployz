import { startRegistration } from "@simplewebauthn/browser";
import { useMutation, useQueryClient, useQueryErrorResetBoundary, useSuspenseQuery } from "@tanstack/react-query";
import { CatchBoundary, getRouteApi, type ErrorComponentProps } from "@tanstack/react-router";
import { KeyRoundIcon } from "lucide-react";
import { Suspense, useState } from "react";
import { RouteErrorAlert } from "#/components/route-error-alert";
import { SettingsList, SettingsListItem } from "#/components/settings/settings-list";
import { SettingsSection } from "#/components/settings/settings-section";
import { Alert, AlertDescription, AlertTitle } from "#/components/ui/alert";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Skeleton } from "#/components/ui/skeleton";
import { isPublicError } from "#/lib/error-message";
import { beginPasskeyServerFn, finishPasskeyServerFn } from "#/modules/passkeys/passkeys.functions";
import { passkeysQueryOptions, type PasskeyRow, type PasskeysView, type ServerTrust, trustedEverywhere } from "#/modules/passkeys/passkeys.queries";
import { RelativeTime } from "#/components/relative-time";

const organizationRoute = getRouteApi("/_protected/cloud/$organizationSlug");

/**
 * Passkeys that let you approve changes from this browser. Adding one here only files it, and each row says which
 * Servers report they trust it.
 */
export function PasskeysSection() {
  const { organizationSlug } = organizationRoute.useParams();
  const { access } = organizationRoute.useLoaderData();
  return access === "member" ? <MemberPasskeys organizationSlug={organizationSlug} /> : null;
}

function MemberPasskeys({ organizationSlug }: { organizationSlug: string }) {
  const queryClient = useQueryClient();
  const [failure, setFailure] = useState<string | null>(null);
  const add = useMutation({
    mutationFn: async () => {
      const options = await beginPasskeyServerFn({ data: { organizationSlug } });
      const response = await startRegistration({ optionsJSON: options });
      return finishPasskeyServerFn({ data: { organizationSlug, response: JSON.stringify(response) } });
    },
    onMutate: () => setFailure(null),
    onError: (error) => setFailure(isPublicError(error) && error.code === "CONFLICT" ? error.message : "browser"),
    // A read still out was asked before this passkey existed, so drop it and read again; also after a failure, since
    // Cloud may have filed the passkey and lost only its answer.
    onSettled: () => {
      const list = passkeysQueryOptions(organizationSlug);
      void queryClient.cancelQueries({ queryKey: list.queryKey }).then(() => queryClient.prefetchQuery(list));
    },
  });
  return (
    <SettingsSection id="passkeys" title="Passkeys"
      description="Approve changes from this browser."
      action={(
        <Button variant="outline" size="sm" disabled={add.isPending} onClick={() => add.mutate()}>
          Add passkey
        </Button>
      )}>
      {failure === null ? null : <AddFailed message={failure} />}
      {/* Each Server is asked live what it says of a passkey, so the list lands after the page. */}
      <CatchBoundary getResetKey={() => organizationSlug} errorComponent={PasskeyListError}>
        <Suspense fallback={<Skeleton className="h-12 w-full" />}>
          <MemberPasskeyList organizationSlug={organizationSlug} />
        </Suspense>
      </CatchBoundary>
    </SettingsSection>
  );
}

function MemberPasskeyList({ organizationSlug }: { organizationSlug: string }) {
  const { data } = useSuspenseQuery(passkeysQueryOptions(organizationSlug));
  return <PasskeyList view={data} />;
}

function PasskeyListError({ reset }: ErrorComponentProps) {
  const queries = useQueryErrorResetBoundary();
  return (
    <RouteErrorAlert title="Passkeys couldn’t load" description="Your passkeys are unavailable right now. Try loading them again."
      onRetry={() => { queries.reset(); reset(); }} />
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
        Try another browser, or approve from the CLI:
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

