import { useSuspenseQuery } from "@tanstack/react-query";
import { useLiveSuspenseQuery } from "@tanstack/react-db";
import { useRouteContext } from "@tanstack/react-router";
import { BotIcon, MonitorIcon, TerminalIcon } from "lucide-react";
import { toast } from "sonner";
import { prefetchRemote } from "#/collections/route-data";
import type { CollectionScope } from "#/collections/scope";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { GitHubMarkIcon } from "#/components/icons/github-mark";
import { SettingsList, SettingsListItem } from "#/components/settings/settings-list";
import { SettingsSection } from "#/components/settings/settings-section";
import { Avatar, AvatarFallback, AvatarImage } from "#/components/ui/avatar";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { distinctNames } from "#/lib/distinct-names";
import {
  accountSessionsRead,
  connectedAgentsRead,
  disconnectAgent,
  getAccountSessionsCollection,
  getConnectedAgentsCollection,
  getLinkedAccountsCollection,
  linkedAccountsRead,
  revokeAccountSession,
  type AccountSessionRow,
  type ConnectedAgentRow,
} from "#/modules/account/account.collection";
import { notificationSettingsQueryOptions, useSetNotificationChannel } from "#/modules/notifications/notifications.queries";
import { formatRelativeTime } from "#/utils/relative-time";
import { RelativeTime, useClockReadable } from "#/components/relative-time";
import { NotificationsSection } from "./notifications-section";
import { formatDay } from "#/lib/format-day";

export function prefetchAccount(scope: CollectionScope) {
  return prefetchRemote(scope, notificationSettingsQueryOptions(), accountSessionsRead(scope), linkedAccountsRead(scope), connectedAgentsRead(scope));
}

/** The whole Account page: inside an Organization, or on its own for a user in none. */
export function AccountSections() {
  return (
    <div className="flex flex-col gap-6">
      <ProfileSection />
      <SessionsSection />
      <ConnectedAgentsSection />
      <AccountNotificationsSection />
    </div>
  );
}

const providerLabel = (providerId: string) => (providerId === "github" ? "GitHub" : providerId);

/** Who you are, and how you sign in. Both come from GitHub, so nothing here is edited. */
function ProfileSection() {
  const { session } = useRouteContext({ from: "/_protected" });
  const { data: linked } = useLiveSuspenseQuery(getLinkedAccountsCollection(useCollectionScope()));
  const { user } = session;
  return (
    <SettingsSection id="profile" title="Profile">
      <SettingsList label="Profile">
        <SettingsListItem
          media={(
            <Avatar>
              {user.image ? <AvatarImage src={user.image} alt="" /> : null}
              <AvatarFallback>{user.name.slice(0, 1).toUpperCase()}</AvatarFallback>
            </Avatar>
          )}
          label={user.name}
          hint={user.email}
          commit="none"
        />
        {linked.map((row) => (
          <SettingsListItem
            key={row.id}
            media={<GitHubMarkIcon />}
            label={providerLabel(row.providerId)}
            hint={`Linked ${formatDay(row.createdAt)}`}
            commit="none"
            actions={<Badge variant="outline">Sign-in</Badge>}
          />
        ))}
      </SettingsList>
    </SettingsSection>
  );
}

/** Every place you're signed in. Browsers end here; the CLI signs out with `ployz logout`, which also drops its Server access. */
function SessionsSection() {
  const scope = useCollectionScope();
  const { data } = useLiveSuspenseQuery(getAccountSessionsCollection(scope));
  // A collection iterates by key, not in the server's order.
  const sessions = [...data].sort((a, b) => b.lastActiveAt.localeCompare(a.lastActiveAt));
  return (
    <SettingsSection id="sessions" title="Sessions"
      description={<>CLI devices sign out with <code className="font-mono">ployz logout</code>.</>}>
      <SessionList sessions={sessions} currentSessionId={scope.sessionId} onSignOut={(id) => {
        revokeAccountSession(scope, id).catch(() => toast.error("Couldn't sign out that session."));
      }} />
    </SettingsSection>
  );
}

export function SessionList({ sessions, currentSessionId, onSignOut }: {
  sessions: readonly AccountSessionRow[];
  currentSessionId: string;
  onSignOut: (session: string) => void;
}) {
  const clock = useClockReadable();
  const signOutable = sessions.filter((row) => row.id !== currentSessionId && !row.cli);
  const names = distinctNames(signOutable.map((row) =>
    [deviceLabel(row), row.ipAddress, clock ? `active ${formatRelativeTime(new Date(row.lastActiveAt))}` : null]));
  const signOutNames = new Map(signOutable.map((row, index) => [row.id, names[index]]));
  return (
    <SettingsList label="Sessions">
      {sessions.map((row) => (
        <SettingsListItem
          key={row.id}
          media={row.cli ? <TerminalIcon /> : <MonitorIcon />}
          label={deviceLabel(row)}
          hint={<>{row.ipAddress ? `${row.ipAddress} · ` : null}Active <RelativeTime date={new Date(row.lastActiveAt)} /></>}
          commit={row.id === currentSessionId || row.cli ? "none" : "change"}
          actions={row.id === currentSessionId
            ? <Badge variant="secondary">This device</Badge>
            : row.cli
              ? <Badge variant="outline">CLI</Badge>
              : (
                <Button variant="outline" size="sm" aria-label={`Sign out ${signOutNames.get(row.id)}`}
                  onClick={() => onSignOut(row.id)}>
                  Sign out
                </Button>
              )}
        />
      ))}
    </SettingsList>
  );
}

/** The agents you allowed on /connect. Self-hosted Cloud takes none, so the section shows only once there is one. */
function ConnectedAgentsSection() {
  const scope = useCollectionScope();
  const { data } = useLiveSuspenseQuery(getConnectedAgentsCollection(scope));
  if (data.length === 0) return null;
  // A collection iterates by key, not in the server's order.
  const agents = [...data].sort((a, b) => b.allowedAt.localeCompare(a.allowedAt));
  return (
    <SettingsSection id="connected-agents" title="Connected agents"
      description="Disconnecting an agent stops it at once.">
      <ConnectedAgentList agents={agents} onDisconnect={(id) => {
        disconnectAgent(scope, id).catch(() => toast.error("Couldn't disconnect that agent."));
      }} />
    </SettingsSection>
  );
}

export function ConnectedAgentList({ agents, onDisconnect }: {
  agents: readonly ConnectedAgentRow[];
  onDisconnect: (consent: string) => void;
}) {
  const clock = useClockReadable();
  const names = distinctNames(agents.map((row) =>
    [agentLabel(row), row.organization, clock ? `allowed ${formatRelativeTime(new Date(row.allowedAt))}` : null]));
  return (
    <SettingsList label="Connected agents">
      {agents.map((row, index) => (
        <SettingsListItem
          key={row.id}
          media={<BotIcon />}
          label={agentLabel(row)}
          hint={<>{row.organization} · Allowed <RelativeTime date={new Date(row.allowedAt)} /></>}
          commit="change"
          actions={(
            <Button variant="outline" size="sm" aria-label={`Disconnect ${names[index]}`} onClick={() => onDisconnect(row.id)}>
              Disconnect
            </Button>
          )}
        />
      ))}
    </SettingsList>
  );
}

const agentLabel = (row: ConnectedAgentRow) => row.name ?? "Unnamed agent";

function AccountNotificationsSection() {
  const { data: settings } = useSuspenseQuery(notificationSettingsQueryOptions());
  const setChannel = useSetNotificationChannel();
  return <NotificationsSection settings={settings} onChange={(change) => setChannel.mutate(change)} />;
}

/** "Chrome on macOS", or "Ployz CLI 0.2.4". */
function deviceLabel(row: AccountSessionRow) {
  const agent = row.userAgent ?? "";
  if (row.cli) return `Ployz CLI ${agent.split(/[/\s]/)[1] ?? ""}`.trim();
  const browser = /Edg(iOS)?\//.test(agent) ? "Edge" : /F(irefox|xiOS)\//.test(agent) ? "Firefox" : /C(hrome|riOS)\//.test(agent) ? "Chrome" : /Safari\//.test(agent) ? "Safari" : null;
  const os = /iPhone|iPad/.test(agent) ? "iOS" : /Android/.test(agent) ? "Android" : /Mac OS X/.test(agent) ? "macOS" : /Windows/.test(agent) ? "Windows" : /Linux/.test(agent) ? "Linux" : null;
  if (browser && os) return `${browser} on ${os}`;
  return browser ?? os ?? "Unknown device";
}
