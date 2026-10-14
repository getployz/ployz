import { useSuspenseQuery } from "@tanstack/react-query";
import { useLiveSuspenseQuery } from "@tanstack/react-db";
import { Link, useRouteContext } from "@tanstack/react-router";
import { toast } from "sonner";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { Avatar, AvatarFallback, AvatarImage } from "#/components/ui/avatar";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Field, FieldContent, FieldDescription, FieldTitle } from "#/components/ui/field";
import type { AccountSection } from "#/components/dashboard-navigation-model";
import { cn } from "#/lib/utils";
import { getAccountSessionsCollection, getLinkedAccountsCollection, revokeAccountSession, type AccountSessionRow } from "#/modules/account/account.collection";
import { notificationSettingsQueryOptions, useSetNotificationChannel } from "#/modules/notifications/notifications.queries";
import { SettingsSection } from "#/routes/_protected/cloud/$organizationSlug/-components/SettingsSection";
import { formatRelativeTime } from "#/utils/relative-time";
import { NotificationsSection } from "./notifications-section";

const providerLabel = (providerId: string) => (providerId === "github" ? "GitHub" : providerId);

const dateFormat = new Intl.DateTimeFormat("en-US", { dateStyle: "medium" });

/** Who you are, and how you sign in. Both come from GitHub, so nothing here is edited. */
export function ProfileSection() {
  const { session } = useRouteContext({ from: "/_protected" });
  const { data: linked } = useLiveSuspenseQuery(getLinkedAccountsCollection(useCollectionScope()));
  const { user } = session;
  return (
    <SettingsSection id="profile" title="Profile">
      <Field orientation="responsive">
        <FieldContent>
          <div className="flex items-center gap-3">
            <Avatar className="size-10">
              {user.image ? <AvatarImage src={user.image} alt="" /> : null}
              <AvatarFallback>{user.name.slice(0, 1).toUpperCase()}</AvatarFallback>
            </Avatar>
            <div className="min-w-0">
              <FieldTitle className="truncate">{user.name}</FieldTitle>
              <FieldDescription className="truncate">{user.email}</FieldDescription>
            </div>
          </div>
        </FieldContent>
      </Field>
      {linked.map((row) => (
        <Field key={row.id} orientation="responsive">
          <FieldContent>
            <FieldTitle>{providerLabel(row.providerId)}</FieldTitle>
            <FieldDescription>Linked {dateFormat.format(new Date(row.createdAt))}</FieldDescription>
          </FieldContent>
          <div><Badge variant="outline">Sign-in</Badge></div>
        </Field>
      ))}
    </SettingsSection>
  );
}

/** Every place you're signed in. Browsers end here; the CLI signs out with `ployz logout`, which also drops its Server access. */
export function SessionsSection() {
  const scope = useCollectionScope();
  const { data: sessions } = useLiveSuspenseQuery(getAccountSessionsCollection(scope));
  return (
    <SettingsSection id="sessions" title="Sessions"
      description={<>CLI devices sign out with <code className="font-mono text-xs">ployz logout</code>.</>}>
      {sessions.map((row) => (
        <Field key={row.id} orientation="responsive">
          <FieldContent>
            <FieldTitle>{deviceLabel(row)}</FieldTitle>
            <FieldDescription>
              {[row.ipAddress, `Active ${formatRelativeTime(new Date(row.lastActiveAt))}`].filter(Boolean).join(" · ")}
            </FieldDescription>
          </FieldContent>
          {/* A wrapper, so the stacked phone layout stretches it and not the badge. */}
          <div>
            {row.id === scope.sessionId
              ? <Badge variant="success">This device</Badge>
              : row.cli
                ? <Badge variant="outline">CLI</Badge>
                : (
                  <Button variant="outline" size="sm" onClick={() => {
                    revokeAccountSession(scope, row.id).catch(() => toast.error("Couldn't sign out that session."));
                  }}>
                    Sign out
                  </Button>
                )}
          </div>
        </Field>
      ))}
    </SettingsSection>
  );
}

export function AccountNotificationsSection() {
  const { data: settings } = useSuspenseQuery(notificationSettingsQueryOptions());
  const setChannel = useSetNotificationChannel();
  return <NotificationsSection settings={settings} onChange={(change) => setChannel.mutate(change)} />;
}

export function AccountSectionView({ section }: { section: AccountSection }) {
  switch (section) {
    case "profile":
      return <ProfileSection />;
    case "sessions":
      return <SessionsSection />;
    case "notifications":
      return <AccountNotificationsSection />;
  }
}

/** Prototype only: flips between the layout variants. A and C live in an Organization's shell, so they need one. */
export function VariantSwitcher({ current }: { current: "a" | "b" | "c" }) {
  const { session } = useRouteContext({ from: "/_protected" });
  const slug = session.session.activeOrganizationSlug;
  const pill = (variant: "a" | "b" | "c") => cn(
    "rounded-full px-2.5 py-1 font-medium uppercase",
    variant === current ? "bg-foreground text-background" : "text-muted-foreground hover:text-foreground",
  );
  return (
    <nav aria-label="Prototype variant"
      className="fixed right-4 bottom-20 z-50 flex items-center gap-1 rounded-full border bg-popover p-1 text-xs shadow-md md:bottom-4">
      <span className="px-2 text-muted-foreground">Variant</span>
      {slug ? <Link to="/cloud/$organizationSlug/~/account" params={{ organizationSlug: slug }} search={{ variant: "a" }} className={pill("a")}>a</Link> : null}
      <Link to="/cloud/account" search={{ variant: "b" }} className={pill("b")}>b</Link>
      {slug ? <Link to="/cloud/$organizationSlug/~/account" params={{ organizationSlug: slug }} search={{ variant: "c" }} className={pill("c")}>c</Link> : null}
    </nav>
  );
}

/** "Chrome on macOS", or "Ployz CLI 0.2.4". */
function deviceLabel(row: AccountSessionRow) {
  const agent = row.userAgent ?? "";
  if (row.cli) return `Ployz CLI ${agent.split(/[/\s]/)[1] ?? ""}`.trim();
  const browser = /Edg\//.test(agent) ? "Edge" : /Firefox\//.test(agent) ? "Firefox" : /Chrome\//.test(agent) ? "Chrome" : /Safari\//.test(agent) ? "Safari" : null;
  const os = /iPhone|iPad/.test(agent) ? "iOS" : /Android/.test(agent) ? "Android" : /Mac OS X/.test(agent) ? "macOS" : /Windows/.test(agent) ? "Windows" : /Linux/.test(agent) ? "Linux" : null;
  if (browser && os) return `${browser} on ${os}`;
  return browser ?? os ?? "Unknown device";
}
