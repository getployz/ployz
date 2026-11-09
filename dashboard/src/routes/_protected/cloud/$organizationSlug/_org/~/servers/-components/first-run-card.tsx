import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "#/components/ui/card";
import { ClusterSetup } from "./cluster-setup";
import { InstallTabs } from "./copy-block";
import { useMintFleetToken } from "./use-waiting";

/**
 * First run: mint a token in place, show the line once, and wait for a server to check in. Once one arrives the card
 * shrinks to the line, if this visit minted it, so the user can still run it on more servers.
 */
export function FirstRunCard({ organizationSlug, arrived }: { organizationSlug: string; arrived: boolean }) {
  const mint = useMintFleetToken(organizationSlug);
  if (arrived) {
    return mint.data === undefined ? (
      <p className="text-sm text-muted-foreground">Add servers to queue more here.</p>
    ) : (
      <div className="flex flex-col gap-3">
        <InstallTabs target={mint.data} />
        <p className="text-sm text-muted-foreground">Run it on more servers and they queue above.</p>
      </div>
    );
  }
  return (
    <Card>
      <CardHeader>
        <CardTitle>Add your first server</CardTitle>
        <CardDescription>Run this on any Linux server with root. It shows up below in a few seconds.</CardDescription>
      </CardHeader>
      <CardContent>
        <ClusterSetup organizationSlug={organizationSlug} mint={mint} />
      </CardContent>
    </Card>
  );
}
