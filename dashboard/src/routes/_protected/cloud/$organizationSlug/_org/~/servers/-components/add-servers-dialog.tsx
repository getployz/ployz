import { useState, type ReactNode } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { PlusIcon } from "lucide-react";
import { toast } from "sonner";
import { Button } from "#/components/ui/button";
import type { ButtonVariants } from "#/components/ui/button-variants";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "#/components/ui/dialog";
import { Input } from "#/components/ui/input";
import { Spinner } from "#/components/ui/spinner";
import { Switch } from "#/components/ui/switch";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "#/components/ui/tabs";
import { toErrorMessage } from "#/lib/error-message";
import { FLEET_TOKEN_LIFETIMES, type FleetTokenLifetime } from "#/modules/machines/waiting/fleet-token";
import { createFleetTokenServerFn } from "#/modules/machines/waiting/fleet-token.functions";
import { installLines, cliLifetimeLabel } from "#/modules/machines/waiting/install-lines";
import { fleetTokenKeys, waitingServersQueryOptions } from "#/modules/machines/waiting/waiting.queries";
import { CopyBlock } from "./copy-block";

const LIFETIME_LABELS = { "1d": "1 day", "7d": "7 days", "30d": "30 days" } satisfies Record<FleetTokenLifetime, string>;
const NAME_PATTERN = /^[a-z0-9][a-z0-9-]{0,62}$/;

type Minted = { readonly name: string; readonly token: string; readonly cloudOrigin: string; readonly expiresIn: FleetTokenLifetime; readonly autoMax: number | null };

export function AddServersDialog({ organizationSlug, label = "Add servers", variant = "ink" }: {
  organizationSlug: string;
  label?: ReactNode;
  variant?: ButtonVariants["variant"];
}) {
  const [open, setOpen] = useState(false);
  return (
    <>
      <Button type="button" variant={variant} onClick={() => setOpen(true)}>
        <PlusIcon data-icon="inline-start" />
        {label}
      </Button>
      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent className="sm:max-w-xl">
          {open ? <MintFlow organizationSlug={organizationSlug} onDone={() => setOpen(false)} /> : null}
        </DialogContent>
      </Dialog>
    </>
  );
}

function MintFlow({ organizationSlug, onDone }: { organizationSlug: string; onDone: () => void }) {
  const queryClient = useQueryClient();
  const [name, setName] = useState("");
  const [expiresIn, setExpiresIn] = useState<FleetTokenLifetime>("1d");
  const [auto, setAuto] = useState(false);
  const [max, setMax] = useState("10");
  const maxServers = Number.parseInt(max, 10);
  const nameOk = name.trim() === "" || NAME_PATTERN.test(name.trim());
  const maxOk = !auto || (Number.isInteger(maxServers) && maxServers >= 1 && maxServers <= 1000);
  const mint = useMutation({
    mutationFn: () => createFleetTokenServerFn({
      data: {
        organizationSlug,
        name: name.trim() === "" ? null : name.trim(),
        expiresIn,
        autoEnroll: auto ? { maxServers } : null,
      },
    }),
    onError: (cause) => toast.error(toErrorMessage(cause, "Couldn't create the token.")),
    onSettled: () => queryClient.invalidateQueries({ queryKey: fleetTokenKeys.all }),
  });

  if (mint.data !== undefined) {
    return (
      <MintDone
        organizationSlug={organizationSlug}
        minted={{ ...mint.data, expiresIn, autoMax: auto ? maxServers : null }}
        onDone={onDone}
      />
    );
  }
  return (
    <form
      className="flex flex-col gap-4"
      onSubmit={(event) => {
        event.preventDefault();
        if (nameOk && maxOk) mint.mutate();
      }}
    >
      <DialogHeader>
        <DialogTitle>New fleet token</DialogTitle>
        <DialogDescription>Run it on as many servers as you like. They show up here.</DialogDescription>
      </DialogHeader>
      <label className="flex flex-col gap-1.5 text-sm">
        <span className="text-muted-foreground">Name</span>
        <Input
          value={name}
          onChange={(event) => setName(event.target.value)}
          placeholder="Picked for you"
          spellCheck={false}
          aria-invalid={!nameOk}
          className="font-mono"
        />
      </label>
      <div className="flex flex-col gap-1.5 text-sm">
        <span className="text-muted-foreground">Expires</span>
        <div role="group" aria-label="Expires" className="flex w-fit gap-1 rounded-lg bg-muted p-0.75">
          {FLEET_TOKEN_LIFETIMES.map((lifetime) => (
            <button
              key={lifetime}
              type="button"
              aria-pressed={expiresIn === lifetime}
              onClick={() => setExpiresIn(lifetime)}
              className="h-7 rounded-md px-3 text-[13px] text-muted-foreground aria-pressed:bg-background aria-pressed:text-foreground aria-pressed:shadow-sm"
            >
              {LIFETIME_LABELS[lifetime]}
            </button>
          ))}
        </div>
      </div>
      <div className="flex items-start gap-3 text-sm">
        <Switch id="enroll-on-arrival" className="mt-0.5" checked={auto} onCheckedChange={setAuto} />
        <div className="flex flex-col gap-0.5">
          <label htmlFor="enroll-on-arrival" className="font-medium">Enroll on arrival</label>
          <span className="text-muted-foreground">Skip the review. They join as they arrive.</span>
          {auto ? (
            <span className="mt-2 flex items-center gap-2 text-muted-foreground">
              Up to
              <Input
                aria-label="Up to how many servers"
                inputMode="numeric"
                value={max}
                onChange={(event) => setMax(event.target.value)}
                aria-invalid={!maxOk}
                className="h-7 w-16"
              />
              servers
            </span>
          ) : null}
        </div>
      </div>
      <DialogFooter>
        <Button type="button" variant="outline" onClick={onDone}>Cancel</Button>
        <Button type="submit" variant="ink" disabled={!nameOk || !maxOk || mint.isPending}>
          {mint.isPending ? <Spinner data-icon="inline-start" /> : null}
          Create
        </Button>
      </DialogFooter>
    </form>
  );
}

function MintDone({ organizationSlug, minted, onDone }: { organizationSlug: string; minted: Minted; onDone: () => void }) {
  const lines = installLines(minted.cloudOrigin, minted.token);
  return (
    <div className="flex flex-col gap-3">
      <DialogHeader>
        <DialogTitle className="font-mono">{minted.name}</DialogTitle>
        <DialogDescription>{minted.autoMax === null ? "Waits for you" : `Enrolls on arrival, up to ${minted.autoMax}`}</DialogDescription>
      </DialogHeader>
      <Tabs defaultValue="shell">
        <TabsList>
          <TabsTrigger value="shell">Shell</TabsTrigger>
          <TabsTrigger value="cloud-init">cloud-init</TabsTrigger>
          <TabsTrigger value="terraform">Terraform</TabsTrigger>
        </TabsList>
        <TabsContent value="shell"><CopyBlock value={lines.shell} /></TabsContent>
        <TabsContent value="cloud-init"><CopyBlock value={lines.cloudInit} /></TabsContent>
        <TabsContent value="terraform"><CopyBlock value={lines.terraform} /></TabsContent>
      </Tabs>
      <div className="flex flex-col gap-0.5 text-sm">
        <p className="text-muted-foreground">
          {minted.autoMax === null
            ? "Installs Docker and Ployz. Nothing joins until you say so."
            : `Installs Docker and Ployz, then joins. Stops after ${minted.autoMax}.`}
        </p>
        <p className="text-warning">Shown once. Expires in {cliLifetimeLabel(minted.expiresIn)}.</p>
      </div>
      <Arrivals organizationSlug={organizationSlug} token={minted.name} onDone={onDone} />
    </div>
  );
}

function Arrivals({ organizationSlug, token, onDone }: { organizationSlug: string; token: string; onDone: () => void }) {
  const { data } = useQuery(waitingServersQueryOptions(organizationSlug));
  const here = data?.filter((server) => server.fleetToken === token).length ?? 0;
  return (
    <div className="flex h-11 items-center gap-2.5 rounded-md border px-3 text-sm">
      {here === 0 ? (
        <>
          <Spinner />
          <span className="text-muted-foreground">Listening for servers…</span>
        </>
      ) : (
        <>
          <span><span className="font-medium tabular-nums">{here}</span> {here === 1 ? "server is" : "servers are"} here</span>
          <Button size="sm" variant="outline" className="ml-auto" onClick={onDone}>Show</Button>
        </>
      )}
    </div>
  );
}
