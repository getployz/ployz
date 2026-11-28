import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { CopyButton } from "#/components/copy-button";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "#/components/ui/tabs";
import { installLines, type InstallTarget } from "#/modules/machines/installer";
import { installKeyQueryOptions } from "#/modules/machines/waiting/passkey-join.queries";
import type { InstallKey } from "#/modules/passkeys/passkeys.server";

export function CopyBlock({ value }: { value: string }) {
  return (
    <div className="relative">
      <pre className="ph-no-capture max-h-56 overflow-auto rounded-md border border-border bg-muted p-3 pr-24 font-mono text-xs leading-relaxed text-foreground">
        <code className="whitespace-pre-wrap break-all">{value}</code>
      </pre>
      <CopyButton value={value} showLabel variant="outline" className="absolute top-2 right-2" />
    </div>
  );
}

/**
 * The install command for `target`, carrying the passkey the new Server will trust to found or join the Cluster. When
 * the Servers trust no passkey of yours, a line can't carry one that works, so it says whose passkey works instead.
 */
export function InstallTabs({ organizationSlug, target }: { organizationSlug: string; target: InstallTarget }) {
  const key = useQuery(installKeyQueryOptions(organizationSlug));
  if (key.isPending) {
    return <p role="status" className="text-sm text-muted-foreground">Checking your passkeys…</p>;
  }
  if (key.isError) return <p role="alert" className="text-sm text-destructive">Couldn't check your passkeys. Try again.</p>;
  return <KeyedInstall organizationSlug={organizationSlug} target={target} installKey={key.data} />;
}

export function KeyedInstall({ organizationSlug, target, installKey }: { organizationSlug: string; target: InstallTarget; installKey: InstallKey }) {
  if (installKey.kind === "unreachable") {
    return <p role="status" className="text-sm text-muted-foreground">Your servers can't be reached. Try again when they're online.</p>;
  }
  if (installKey.kind === "untrusted") {
    return (
      <div className="flex flex-col gap-2 text-sm">
        <p>
          Your servers don't trust a passkey of yours yet.{" "}
          {installKey.trustedBy.length > 0
            ? `${installKey.trustedBy.join(", ")} can add servers. Your servers trust their passkey.`
            : "Add servers with ployz server add from an approved device."}
        </p>
        {installKey.yours === null ? null : (
          <>
            <p className="text-muted-foreground">To trust your passkey, run this as root on each server:</p>
            <CopyBlock value={`sudo ployz key add ${installKey.yours}`} />
          </>
        )}
      </div>
    );
  }
  if (installKey.kind === "no_passkey") {
    return (
      <p role="status" className="text-sm">
        Add a passkey before your first server. Your new cluster trusts it from the start, so it never runs without a key.{" "}
        <Link to="/cloud/$organizationSlug/~/account" params={{ organizationSlug }} className="underline">Add a passkey</Link>, then
        come back here.
      </p>
    );
  }
  const lines = installLines(target, installKey.key);
  return (
    <div className="flex flex-col gap-2">
      <Tabs defaultValue="shell">
        <TabsList>
          <TabsTrigger value="shell">Shell</TabsTrigger>
          <TabsTrigger value="cloud-init">cloud-init</TabsTrigger>
        </TabsList>
        <TabsContent value="shell" className="flex flex-col gap-2">
          <CopyBlock value={lines.shell} />
          <p className="text-sm text-muted-foreground">Paste this Fleet Token when it asks. It doesn't show what you paste.</p>
          <CopyBlock value={target.token} />
        </TabsContent>
        <TabsContent value="cloud-init"><CopyBlock value={lines.cloudInit} /></TabsContent>
      </Tabs>
      <p className="text-sm text-muted-foreground">The server will trust your passkey, {installKey.account}.</p>
    </div>
  );
}
