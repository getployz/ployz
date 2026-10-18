import { TerminalIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "#/components/ui/card";
import { Item, ItemContent, ItemDescription, ItemGroup, ItemMedia, ItemTitle } from "#/components/ui/item";
import { Separator } from "#/components/ui/separator";
import { Spinner } from "#/components/ui/spinner";
import { cliLifetimeLabel, installLines } from "#/modules/machines/waiting/install-lines";
import { InstallTabs } from "./copy-block";
import { useMintFleetToken } from "./use-waiting";

const STEPS = [
  { title: "Run the line", description: "Installs the agent. Docker too, if missing." },
  { title: "Review", description: "See its size, disks and blockers before it joins." },
  { title: "Enroll", description: "Sets up storage and networking, then it's ready for deploys." },
] as const;

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
        <InstallTabs lines={installLines(mint.data.cloudOrigin, mint.data.token)} />
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
      <CardContent className="flex flex-col gap-4">
        {mint.data === undefined ? (
          <Button
            variant="ink"
            className="w-fit"
            disabled={mint.isPending}
            onClick={() => mint.mutate({ name: null, expiresIn: "7d", autoEnroll: null })}
          >
            {mint.isPending ? <Spinner data-icon="inline-start" /> : <TerminalIcon data-icon="inline-start" />}
            Get install command
          </Button>
        ) : (
          <div className="flex flex-col gap-3">
            <InstallTabs lines={installLines(mint.data.cloudOrigin, mint.data.token)} />
            <p className="text-sm text-warning">Shown once. Expires in {cliLifetimeLabel("7d")}.</p>
            <div className="flex items-center gap-2 text-sm text-muted-foreground">
              <Spinner />
              Listening for servers…
            </div>
          </div>
        )}
        <Separator />
        <ItemGroup className="grid gap-2 sm:grid-cols-3">
          {STEPS.map((step, index) => (
            <Item key={step.title} size="sm">
              <ItemMedia variant="icon">{index + 1}</ItemMedia>
              <ItemContent>
                <ItemTitle>{step.title}</ItemTitle>
                <ItemDescription>{step.description}</ItemDescription>
              </ItemContent>
            </Item>
          ))}
        </ItemGroup>
      </CardContent>
    </Card>
  );
}
