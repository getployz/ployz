import { useState } from "react";
import { useQuery, useQueryClient, useSuspenseQuery } from "@tanstack/react-query";
import { useServerFn } from "@tanstack/react-start";
import { toast } from "sonner";
import { GitHubMarkIcon } from "#/components/icons/github-mark";
import { RelativeTime } from "#/components/relative-time";
import { SettingsEmpty, SettingsForm, SettingsList, SettingsListItem } from "#/components/settings/settings-list";
import { SettingsFoot, SettingsRow, SettingsSection } from "#/components/settings/settings-section";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "#/components/ui/alert-dialog";
import { Avatar, AvatarFallback, AvatarImage } from "#/components/ui/avatar";
import { Button } from "#/components/ui/button";
import { Checkbox } from "#/components/ui/checkbox";
import { toErrorMessage } from "#/lib/error-message";
import type { ConnectNotice } from "#/modules/github/github-connect";
import { acceptGithubOfferServerFn, disconnectGithubAccountServerFn } from "#/modules/github/github-connect.functions";
import { githubOfferQueryOptions, organizationGithubQueryOptions } from "#/modules/github/github-connect.queries";
import { githubKeys } from "#/modules/github/github.queries";

const NOTICES = {
  linked: { title: "GitHub connected", description: "Repositories appear in a moment.", tone: "default" },
  none: { title: "Nothing new to connect", description: "Every installation you can reach is already here.", tone: "default" },
  "not-yours": { title: "Not connected", description: "GitHub doesn't list that installation for you.", tone: "destructive" },
  "not-admin": { title: "Not an owner", description: "Only an owner of a GitHub account can connect it.", tone: "destructive" },
  pending: { title: "Waiting on GitHub", description: "An owner of that GitHub account has to approve the app. Then use an existing installation.", tone: "default" },
  failed: { title: "GitHub didn't finish", description: "Nothing was connected. Try again.", tone: "destructive" },
} satisfies Record<ConnectNotice, { title: string; description: string; tone: "default" | "destructive" }>;

function connectHref(organizationSlug: string, existing: boolean) {
  const params = new URLSearchParams({ organization: organizationSlug });
  if (existing) params.set("existing", "1");
  return `/api/github/connect?${params.toString()}`;
}

export function GithubSettings({ organizationSlug, notice, offer, clearConnectSearch }: {
  organizationSlug: string;
  notice: ConnectNotice | undefined;
  offer: string | undefined;
  clearConnectSearch: () => void;
}) {
  const { data } = useSuspenseQuery({
    ...organizationGithubQueryOptions(organizationSlug),
    refetchInterval: (query) => query.state.data?.accounts.some(awaitsFirstSync) ? 2_500 : false,
  });
  const offered = useQuery({ ...githubOfferQueryOptions(organizationSlug, offer ?? ""), enabled: offer !== undefined && data.canManage });
  const shown = notice === undefined ? null : NOTICES[notice];
  if (offer !== undefined && data.canManage && !offered.isError) {
    return (
      <SettingsSection id="github" title="GitHub" description="Accounts this organization builds from.">
        <OfferPicker organizationSlug={organizationSlug} offer={offer} accounts={offered.data} clearConnectSearch={clearConnectSearch} />
      </SettingsSection>
    );
  }
  return (
    <SettingsSection id="github" title="GitHub" description="Accounts this organization builds from.">
      {offered.isError ? (
        <SettingsRow
          label="This link expired"
          hint={<span className="text-destructive">Connect GitHub again.</span>}
          commit="none"
          control={<Button variant="ghost" size="sm" onClick={clearConnectSearch}>Dismiss</Button>}
        />
      ) : null}
      {shown && offer === undefined ? (
        <SettingsRow
          label={shown.title}
          hint={shown.tone === "destructive" ? <span className="text-destructive">{shown.description}</span> : shown.description}
          commit="none"
          control={<Button variant="ghost" size="sm" onClick={clearConnectSearch}>Dismiss</Button>}
        />
      ) : null}
      {data.accounts.length === 0 ? (
        <SettingsEmpty
          title="No GitHub accounts yet"
          actions={data.canManage ? <ConnectButtons organizationSlug={organizationSlug} primary /> : undefined}
        >
          {data.canManage ? "Connect one to build from its repositories." : "An owner or admin can connect one."}
        </SettingsEmpty>
      ) : (
        <SettingsList label="GitHub accounts">
          {data.accounts.map((account) => (
            <AccountRow key={account.login} organizationSlug={organizationSlug} account={account} canManage={data.canManage} />
          ))}
        </SettingsList>
      )}
      {data.canManage && data.accounts.length > 0 ? (
        <SettingsFoot>
          <ConnectButtons organizationSlug={organizationSlug} />
        </SettingsFoot>
      ) : null}
    </SettingsSection>
  );
}

/** Connect is the empty card's one call to action; beside a list of accounts it steps back to the foot's outline. */
function ConnectButtons({ organizationSlug, primary }: { organizationSlug: string; primary?: boolean }) {
  return (
    <>
      <Button variant={primary ? "default" : "outline"} size="sm" nativeButton={false} render={<a href={connectHref(organizationSlug, false)} />}>
        <GitHubMarkIcon data-icon="inline-start" />
        Connect GitHub
      </Button>
      <Button variant="ghost" size="sm" nativeButton={false} render={<a href={connectHref(organizationSlug, true)} />}>
        Use existing installation
      </Button>
    </>
  );
}

type Account = {
  login: string;
  avatarUrl: string | null;
  repositories: number;
  repositoriesSynced: boolean;
  connectedBy: string | null;
  connectedAt: string;
};

/** Linked before its first sync landed: zero cached repositories says nothing yet. */
const awaitsFirstSync = (account: Account) => account.repositories === 0 && !account.repositoriesSynced;

function AccountAvatar({ login, avatarUrl }: { login: string; avatarUrl: string | null }) {
  return (
    <Avatar size="sm" aria-hidden>
      {avatarUrl ? <AvatarImage src={avatarUrl} alt="" /> : null}
      <AvatarFallback>{login.slice(0, 2).toUpperCase()}</AvatarFallback>
    </Avatar>
  );
}

function AccountRow({ organizationSlug, account, canManage }: { organizationSlug: string; account: Account; canManage: boolean }) {
  const queryClient = useQueryClient();
  const disconnect = useServerFn(disconnectGithubAccountServerFn);
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);

  async function confirm() {
    setBusy(true);
    try {
      const result = await disconnect({ data: { organizationSlug, login: account.login } });
      setOpen(false);
      toast.success(`Disconnected ${account.login}`, {
        action: { label: "Uninstall on GitHub", onClick: () => window.open(result.uninstall_url, "_blank", "noopener") },
      });
      await queryClient.invalidateQueries({ queryKey: githubKeys.all });
    } catch (error) {
      toast.error(toErrorMessage(error, `Couldn't disconnect ${account.login}.`));
    } finally {
      setBusy(false);
    }
  }

  return (
    <SettingsListItem
      media={<AccountAvatar login={account.login} avatarUrl={account.avatarUrl} />}
      label={account.login}
      hint={(
        <>
          {awaitsFirstSync(account)
            ? "Repositories appear in a moment"
            : account.repositories === 1 ? "1 repository" : `${account.repositories} repositories`}
          {account.connectedBy ? <>, connected by {account.connectedBy}</> : null}
          {" "}<RelativeTime date={new Date(account.connectedAt)} />
        </>
      )}
      commit="none"
      actions={canManage ? (
        <>
          <Button variant="outline" size="sm" onClick={() => setOpen(true)}>Disconnect</Button>
          <AlertDialog open={open} onOpenChange={(next) => { if (!busy) setOpen(next); }}>
            <AlertDialogContent>
              <AlertDialogHeader>
                <AlertDialogTitle>Disconnect {account.login}?</AlertDialogTitle>
                <AlertDialogDescription>
                  Only this organization loses access. Its builds running on GitHub stop. Other organizations keep it.
                </AlertDialogDescription>
              </AlertDialogHeader>
              <AlertDialogFooter>
                <AlertDialogCancel disabled={busy}>Cancel</AlertDialogCancel>
                <AlertDialogAction variant="destructive" disabled={busy} onClick={() => void confirm()}>Disconnect</AlertDialogAction>
              </AlertDialogFooter>
            </AlertDialogContent>
          </AlertDialog>
        </>
      ) : undefined}
    />
  );
}

type OfferAccount = { installationId: number; login: string; avatarUrl: string | null };

function OfferPicker({ organizationSlug, offer, accounts, clearConnectSearch }: {
  organizationSlug: string;
  offer: string;
  accounts: ReadonlyArray<OfferAccount> | undefined;
  clearConnectSearch: () => void;
}) {
  const queryClient = useQueryClient();
  const accept = useServerFn(acceptGithubOfferServerFn);
  const [unpicked, setUnpicked] = useState<ReadonlySet<number>>(new Set());
  const [busy, setBusy] = useState(false);
  const picked = (accounts ?? []).filter((account) => !unpicked.has(account.installationId));

  async function connect() {
    setBusy(true);
    try {
      await accept({ data: { organizationSlug, offer, installationIds: picked.map((account) => account.installationId) } });
      await queryClient.invalidateQueries({ queryKey: githubKeys.all });
      clearConnectSearch();
    } catch (error) {
      toast.error(toErrorMessage(error, "Couldn't connect."));
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      <SettingsRow label="Connect which accounts?" hint="GitHub lists these installations for you." commit="none" />
      {accounts ? (
        <SettingsList label="GitHub accounts to connect">
          {accounts.map((account) => (
            <SettingsListItem
              key={account.installationId}
              pick
              media={<AccountAvatar login={account.login} avatarUrl={account.avatarUrl} />}
              label={account.login}
              commit="form"
              actions={(
                <Checkbox
                  checked={!unpicked.has(account.installationId)}
                  onCheckedChange={(on) => setUnpicked((current) => {
                    const next = new Set(current);
                    if (on) next.delete(account.installationId);
                    else next.add(account.installationId);
                    return next;
                  })}
                />
              )}
            />
          ))}
        </SettingsList>
      ) : null}
      <SettingsForm
        submitLabel={picked.length === 1 ? "Connect 1 account" : `Connect ${picked.length} accounts`}
        onSubmit={() => void connect()}
        onCancel={clearConnectSearch}
        pending={busy}
        disabled={picked.length === 0}
      >
        {null}
      </SettingsForm>
    </>
  );
}
