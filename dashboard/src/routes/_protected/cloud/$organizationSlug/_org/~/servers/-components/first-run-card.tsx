import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "#/components/ui/card";
import { type ClusterNameControl, useClusterName } from "#/modules/cluster-name/cluster-name.hooks";
import { InstallCommand } from "#/modules/machines/add-server/install-command";
import { useMintInstallLine } from "#/modules/machines/add-server/use-install-line";

/** First run: mint one Server's token in place, show its line once, and wait for the Server to found the Cluster. */
export function FirstRun({ organizationSlug }: { organizationSlug: string }) {
  return <FirstRunCard organizationSlug={organizationSlug} cluster={useClusterName(organizationSlug)} />;
}

export function FirstRunCard({ organizationSlug, cluster }: { organizationSlug: string; cluster: ClusterNameControl }) {
  const mint = useMintInstallLine(organizationSlug);
  return (
    <Card>
      <CardHeader>
        <CardTitle>Add your first server</CardTitle>
        <CardDescription>Run one command as root on any Linux server. It installs Ployz, and Docker if it's missing.</CardDescription>
      </CardHeader>
      <CardContent>
        <InstallCommand organizationSlug={organizationSlug} cluster={cluster} mint={mint} />
      </CardContent>
    </Card>
  );
}
