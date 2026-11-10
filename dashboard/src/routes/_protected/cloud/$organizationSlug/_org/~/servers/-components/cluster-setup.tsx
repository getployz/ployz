import { useEffect, useId, useRef, useState } from "react";
import { CheckIcon, ChevronRightIcon, TerminalIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "#/components/ui/collapsible";
import { Field, FieldContent, FieldDescription, FieldLabel } from "#/components/ui/field";
import { Input } from "#/components/ui/input";
import { Item, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemMedia, ItemTitle } from "#/components/ui/item";
import { Separator } from "#/components/ui/separator";
import { Spinner } from "#/components/ui/spinner";
import { Switch } from "#/components/ui/switch";
import { toErrorMessage } from "#/lib/error-message";
import { clusterNameProblem } from "#/modules/cluster-name/cluster-name";
import type { ClusterNameControl } from "#/modules/cluster-name/cluster-name.hooks";
import { normalizeZone, zoneProblem } from "#/modules/cloudflare/zone";
import { checkCloudflareServerFn, listCloudflareZonesServerFn } from "#/modules/ingress/doors.functions";
import { cliLifetimeLabel } from "#/modules/machines/waiting/install-lines";
import { useRoleGate } from "#/modules/organization/members.queries";
import { InstallTabs } from "./copy-block";
import { holdCloudflare } from "./first-cloudflare";
import { abandonCloudflare, type CardCloudflare, installGate } from "./servers-page-state";
import { ADD_SERVERS_REFUSED, type useMintFleetToken } from "./use-waiting";

const STEPS = [
  { title: "Run the line", description: "Installs the agent. Docker too, if missing." },
  { title: "Review", description: "See its size, disks and blockers before it joins." },
  { title: "Enroll", description: "Sets up storage and networking, then it's ready for deploys." },
] as const;

/**
 * The first server's install line, with the Cluster's choices tucked under it: its name, and Cloudflare Tunnel instead
 * of public ports. The Servers page's first-run card and the canvas's Add servers dialog both show it.
 */
export function ClusterSetup({ organizationSlug, cluster, mint, steps = true }: {
  organizationSlug: string;
  cluster: ClusterNameControl;
  /** The caller's, so the line it minted outlives this card. */
  mint: ReturnType<typeof useMintFleetToken>;
  /** The three steps under the line. The dialog leaves them out. */
  steps?: boolean;
}) {
  const gate = useRoleGate(organizationSlug, "servers:add", ADD_SERVERS_REFUSED);
  const operate = useRoleGate(organizationSlug, "servers:operate");
  const saved = cluster.view.name ?? "";
  const [name, setName] = useState(saved);
  const [open, setOpen] = useState(false);
  const [cloudflare, setCloudflare] = useState<CardCloudflare>({ at: "off", abandoned: false });
  const [token, setToken] = useState("");
  const install = installGate(cloudflare);
  const choose = (next: CardCloudflare) => {
    setCloudflare(next);
    if (next.at === "off") setToken("");
  };
  const nameProblem = clusterNameProblem(name);

  // The token moves to the tab's hold only once a line that founds closed exists, so a failed mint leaves nothing waiting.
  const asked = useRef<{ readonly token: string; readonly zone: string } | null>(null);
  const minted = mint.data;
  useEffect(() => {
    if (minted === undefined || asked.current === null) return;
    holdCloudflare(organizationSlug, { kind: "waiting", ...asked.current });
    asked.current = null;
    setToken("");
  }, [minted, organizationSlug]);

  const getLine = () => {
    if (name !== saved && nameProblem === null) cluster.rename(name);
    asked.current = cloudflare.at === "ready" ? { token, zone: cloudflare.zone } : null;
    mint.mutate({ name: null, expiresIn: "1d", autoEnroll: null, foundsClosed: install.foundsClosed });
  };

  const line = gate.reason === ADD_SERVERS_REFUSED ? (
    <p className="text-sm text-muted-foreground">{ADD_SERVERS_REFUSED}</p>
  ) : mint.data === undefined ? (
    <div className="flex flex-col gap-2">
      <Button
        variant="ink"
        className="w-fit"
        disabled={!gate.allowed || mint.isPending || !install.allowed || nameProblem !== null}
        title={gate.reason}
        onClick={getLine}
      >
        {mint.isPending ? <Spinner data-icon="inline-start" /> : <TerminalIcon data-icon="inline-start" />}
        Get install command
      </Button>
      {install.note === null ? null : <p role="status" className="text-sm text-muted-foreground">{install.note}</p>}
    </div>
  ) : (
    <div className="flex flex-col gap-3">
      <InstallTabs target={mint.data} />
      <p className="text-sm text-warning">Shown once. Expires in {cliLifetimeLabel("1d")}.</p>
      <div className="flex items-center gap-2 text-sm text-muted-foreground">
        <Spinner />
        Listening for servers…
      </div>
      {cloudflare.at === "ready" ? (
        <p className="text-sm text-muted-foreground">
          Your first server starts with ports 80 and 443 closed. Once it joins, Ployz sets up the tunnel on{" "}
          <span className="font-mono">{cloudflare.zone}</span>. Keep this tab open until then.
        </p>
      ) : null}
    </div>
  );

  return (
    <div className="flex flex-col gap-4">
      {line}
      {mint.data === undefined && gate.reason !== ADD_SERVERS_REFUSED ? (
        <Collapsible
          open={open}
          onOpenChange={(next) => {
            setOpen(next);
            if (!next && cloudflare.at !== "ready") choose(abandonCloudflare(cloudflare));
          }}
        >
          <CollapsibleTrigger className="group flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground">
            <ChevronRightIcon aria-hidden className="size-3.5 transition-transform group-data-panel-open:rotate-90" />
            Use Cloudflare Tunnel instead of public ports
          </CollapsibleTrigger>
          <CollapsibleContent>
            <div className="mt-3 flex min-w-0 flex-col gap-4 rounded-lg border p-3">
              <ClusterNameField name={name} problem={nameProblem} disabled={!operate.allowed} onName={setName} />
              <CloudflareChoice
                organizationSlug={organizationSlug}
                choice={cloudflare}
                token={token}
                onToken={setToken}
                onChoice={choose}
              />
            </div>
          </CollapsibleContent>
        </Collapsible>
      ) : null}
      {steps ? (
        <>
          <Separator />
          <ItemGroup className="grid gap-2 sm:grid-cols-3">
            {STEPS.map((row, index) => (
              <Item key={row.title} size="sm">
                <ItemMedia variant="icon">{index + 1}</ItemMedia>
                <ItemContent>
                  <ItemTitle>{row.title}</ItemTitle>
                  <ItemDescription>{row.description}</ItemDescription>
                </ItemContent>
              </Item>
            ))}
          </ItemGroup>
        </>
      ) : null}
    </div>
  );
}

function ClusterNameField({ name, problem, disabled, onName }: {
  name: string;
  problem: string | null;
  disabled: boolean;
  onName: (name: string) => void;
}) {
  const id = useId();
  const hint = useId();
  return (
    <Field>
      <FieldLabel htmlFor={id}>Cluster name</FieldLabel>
      <Input
        id={id}
        value={name}
        disabled={disabled}
        spellCheck={false}
        autoComplete="off"
        className="font-mono sm:max-w-xs"
        aria-invalid={problem !== null}
        aria-describedby={hint}
        onChange={(event) => onName(event.target.value)}
      />
      <FieldDescription id={hint} className={problem === null ? undefined : "text-destructive"}>
        {problem ?? <>Private addresses end in <span className="font-mono break-all">.{name}.ployz.internal</span>.</>}
      </FieldDescription>
    </Field>
  );
}

/**
 * The tunnel half of the choice, run in the card: the token finds its zones, then Cloud checks the token can set the
 * tunnel up on the zone picked. Nothing is created until the first Server joins.
 */
export function CloudflareChoice({ organizationSlug, choice, token, onToken, onChoice }: {
  organizationSlug: string;
  choice: CardCloudflare;
  token: string;
  onToken: (token: string) => void;
  onChoice: (choice: CardCloudflare) => void;
}) {
  const operate = useRoleGate(organizationSlug, "servers:operate");
  const switchId = useId();
  const tokenId = useId();
  const zoneId = useId();
  const zonesId = useId();
  const [zone, setZone] = useState("");
  const [busy, setBusy] = useState(false);
  const [refusal, setRefusal] = useState<string | null>(null);
  const on = choice.at !== "off";
  const problem = choice.at === "zone" && zone !== "" ? zoneProblem(zone, choice.zones) : null;

  const run = async (step: () => Promise<void>) => {
    setBusy(true);
    setRefusal(null);
    try {
      await step();
    } catch (error) {
      setRefusal(toErrorMessage(error, "Cloudflare didn't answer. Try again."));
    } finally {
      setBusy(false);
    }
  };
  const findZones = () => run(async () => {
    const found = await listCloudflareZonesServerFn({ data: { organizationSlug, token: token.trim() } });
    if (zone === "" && found.zones.length === 1) setZone(found.zones[0] ?? "");
    onChoice({ at: "zone", zones: found.zones });
  });
  const check = () => run(async () => {
    const checked = await checkCloudflareServerFn({ data: { organizationSlug, token: token.trim(), zone: normalizeZone(zone) } });
    onChoice({ at: "ready", zone: checked.zone });
  });

  return (
    <div className="flex min-w-0 flex-col gap-3">
      <Field orientation="horizontal">
        <Switch
          id={switchId}
          checked={on}
          disabled={!operate.allowed || busy}
          onCheckedChange={(next) => {
            setRefusal(null);
            onChoice(next ? { at: "token" } : abandonCloudflare(choice));
          }}
        />
        <FieldContent>
          <FieldLabel htmlFor={switchId}>Cloudflare Tunnel</FieldLabel>
          <FieldDescription>
            {operate.allowed ? "Your servers open no ports. Visitors arrive through a zone in your own Cloudflare account." : operate.reason}
          </FieldDescription>
        </FieldContent>
      </Field>
      {choice.at === "token" ? (
        <form className="flex min-w-0 flex-col gap-2" onSubmit={(event) => { event.preventDefault(); if (!busy && token.trim() !== "") void findZones(); }}>
          <Field>
            <FieldLabel htmlFor={tokenId}>Cloudflare API token</FieldLabel>
            <div className="flex min-w-0 gap-2">
              <Input id={tokenId} type="password" value={token} autoComplete="off" spellCheck={false} className="min-w-0 font-mono"
                onChange={(event) => onToken(event.target.value)} />
              <Button type="submit" variant="outline" disabled={busy || token.trim() === ""}>{busy ? <Spinner /> : null}Find zones</Button>
            </div>
            <FieldDescription>
              Needs Zone: Zone: Read, Zone: DNS: Edit and Account: Cloudflare Tunnel: Edit. This tab keeps it until your first
              server joins and sets up the tunnel with it. Ployz never stores it.
            </FieldDescription>
          </Field>
        </form>
      ) : null}
      {choice.at === "zone" ? (
        <form className="flex min-w-0 flex-col gap-2" onSubmit={(event) => { event.preventDefault(); if (!busy && zone !== "" && problem === null) void check(); }}>
          <Field>
            <FieldLabel htmlFor={zoneId}>Zone</FieldLabel>
            <div className="flex min-w-0 gap-2">
              <Input id={zoneId} list={zonesId} value={zone} autoComplete="off" spellCheck={false} className="min-w-0 font-mono"
                aria-invalid={problem !== null} onChange={(event) => setZone(event.target.value)} />
              <datalist id={zonesId}>{choice.zones.map((name) => <option key={name} value={name} />)}</datalist>
              <Button type="submit" variant="outline" disabled={busy || zone === "" || problem !== null}>{busy ? <Spinner /> : null}Check</Button>
            </div>
            <FieldDescription className={problem === null ? undefined : "text-destructive"}>
              {problem ?? (choice.zones.length === 0
                ? "This token can't see any zones."
                : <>Apps get names one level below it, like <span className="font-mono break-all">web-production.{zone === "" ? "example.com" : normalizeZone(zone)}</span>.</>)}
            </FieldDescription>
          </Field>
          <Button type="button" variant="ghost" size="sm" className="w-fit" disabled={busy} onClick={() => onChoice({ at: "token" })}>Back</Button>
        </form>
      ) : null}
      {choice.at === "ready" ? (
        <Item variant="outline" size="sm">
          <ItemMedia variant="icon"><CheckIcon className="text-success" /></ItemMedia>
          <ItemContent className="min-w-0">
            <ItemTitle className="break-all">Ready for <span className="font-mono">{choice.zone}</span></ItemTitle>
            <ItemDescription>
              Your token can set it up. Once your first server joins, Ployz creates the tunnel and{" "}
              <span className="font-mono break-all">*.{choice.zone}</span>, then forgets the token.
            </ItemDescription>
          </ItemContent>
          <ItemActions>
            <Button type="button" size="sm" variant="ghost" onClick={() => onChoice({ at: "zone", zones: [choice.zone] })}>Change</Button>
          </ItemActions>
        </Item>
      ) : null}
      {refusal === null ? null : <p role="alert" className="text-sm text-destructive">{refusal}</p>}
    </div>
  );
}
