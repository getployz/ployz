import { useId, useState, type ReactNode } from "react";
import { useQuery } from "@tanstack/react-query";
import { useBlocker } from "@tanstack/react-router";
import { PlusIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import type { ButtonVariants } from "#/components/ui/button-variants";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "#/components/ui/dialog";
import { Field, FieldContent, FieldDescription, FieldGroup, FieldLabel } from "#/components/ui/field";
import { Input } from "#/components/ui/input";
import { Spinner } from "#/components/ui/spinner";
import { Switch } from "#/components/ui/switch";
import { FLEET_TOKEN_LIFETIMES, type FleetTokenLifetime } from "#/modules/machines/waiting/fleet-token";
import { installLines, cliLifetimeLabel } from "#/modules/machines/waiting/install-lines";
import { fleetTokensQueryOptions, waitingServersQueryOptions } from "#/modules/machines/waiting/waiting.queries";
import { useYouMay } from "#/modules/organization/members.queries";
import { InstallTabs } from "./copy-block";
import { ADD_SERVERS_REFUSED, useMintFleetToken } from "./use-waiting";

const LIFETIME_LABELS = { "1d": "1 day", "7d": "7 days", "30d": "30 days" } satisfies Record<FleetTokenLifetime, string>;
const NAME_PATTERN = /^[a-z0-9][a-z0-9-]{0,62}$/;

type Minted = { readonly name: string; readonly token: string; readonly cloudOrigin: string; readonly ployzVersion: string; readonly expiresIn: FleetTokenLifetime; readonly autoEnroll: { readonly maxServers: number } | null };

export function AddServersDialog({ organizationSlug, label = "Add servers", variant = "ink" }: {
  organizationSlug: string;
  label?: ReactNode;
  variant?: ButtonVariants["variant"];
}) {
  const [open, setOpen] = useState(false);
  const mint = useMintFleetToken(organizationSlug);
  const may = useYouMay(organizationSlug, "servers:add");
  useBlocker({ shouldBlockFn: () => true, disabled: !mint.holding });
  return (
    <>
      <Button type="button" variant={variant} disabled={may !== true} title={may === false ? ADD_SERVERS_REFUSED : undefined} onClick={() => { mint.reset(); setOpen(true); }}>
        <PlusIcon data-icon="inline-start" />
        {label}
      </Button>
      <Dialog open={open} onOpenChange={(next) => mint.holding || setOpen(next)}>
        <DialogContent className="sm:max-w-xl">
          {open ? <MintFlow organizationSlug={organizationSlug} mint={mint} onDone={() => setOpen(false)} /> : null}
        </DialogContent>
      </Dialog>
    </>
  );
}

function MintFlow({ organizationSlug, mint, onDone }: { organizationSlug: string; mint: ReturnType<typeof useMintFleetToken>; onDone: () => void }) {
  const [name, setName] = useState("");
  const [expiresIn, setExpiresIn] = useState<FleetTokenLifetime>("1d");
  const [autoEnroll, setAutoEnroll] = useState(false);
  const [maxServers, setMaxServers] = useState("10");
  const max = Number(maxServers);
  const maxOk = !autoEnroll || (Number.isInteger(max) && max >= 1 && max <= 1000);
  const nameOk = name.trim() === "" || NAME_PATTERN.test(name.trim());
  const nameInput = useId();
  const nameRule = useId();

  if (mint.data !== undefined && mint.variables !== undefined) {
    return (
      <MintDone
        organizationSlug={organizationSlug}
        minted={{ ...mint.data, expiresIn: mint.variables.expiresIn, autoEnroll: mint.variables.autoEnroll }}
        onDone={onDone}
      />
    );
  }
  return (
    <form
      className="flex flex-col gap-4"
      onSubmit={(event) => {
        event.preventDefault();
        if (nameOk && maxOk) mint.mutate({ name: name.trim() === "" ? null : name.trim(), expiresIn, autoEnroll: autoEnroll ? { maxServers: max } : null });
      }}
    >
      <DialogHeader>
        <DialogTitle>New fleet token</DialogTitle>
        <DialogDescription>Run it on as many servers as you like. They show up under Servers.</DialogDescription>
      </DialogHeader>
      <div className="flex flex-col gap-1.5 text-sm">
        <label htmlFor={nameInput} className="text-muted-foreground">Name</label>
        <Input
          id={nameInput}
          value={name}
          onChange={(event) => setName(event.target.value)}
          placeholder="Picked for you"
          spellCheck={false}
          aria-invalid={!nameOk}
          aria-describedby={nameOk ? undefined : nameRule}
          className="font-mono"
        />
        {nameOk ? null : <span id={nameRule} className="text-xs text-destructive">Lowercase letters, digits and hyphens, up to 63.</span>}
      </div>
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
      <FieldGroup className="border-t pt-4">
        <Field orientation="horizontal">
          <Switch id="enroll-on-arrival" checked={autoEnroll} onCheckedChange={setAutoEnroll} />
          <FieldContent>
            <FieldLabel htmlFor="enroll-on-arrival">Enroll on arrival</FieldLabel>
            <FieldDescription>Skip the review. They join as they arrive.</FieldDescription>
          </FieldContent>
        </Field>
        {autoEnroll ? (
          <Field>
            <FieldLabel htmlFor="enroll-max">Maximum servers</FieldLabel>
            <Input id="enroll-max" type="number" min={1} max={1000} step={1} value={maxServers} onChange={(event) => setMaxServers(event.target.value)} aria-invalid={!maxOk} className="w-20 tabular-nums" />
            <FieldDescription>Only joins an existing cluster. Enroll the first server yourself.</FieldDescription>
            <FieldDescription className="text-warning">Anyone with this token can join up to {maxOk ? max : "N"} servers before it expires.</FieldDescription>
          </Field>
        ) : null}
      </FieldGroup>
      {mint.stalled ? (
        <div role="alert" className="text-sm text-destructive">
          <p>Ployz hasn't answered. Retry or cancel.</p>
          <p>If it made the token, it shows in Tokens under Servers, where you can revoke it.</p>
        </div>
      ) : null}
      <DialogFooter>
        <Button type="button" variant="outline" disabled={mint.holding} onClick={onDone}>Cancel</Button>
        <Button type="submit" variant="ink" disabled={!nameOk || !maxOk || mint.holding}>
          {mint.holding ? <Spinner data-icon="inline-start" /> : null}
          {mint.stalled ? "Retry" : "Create"}
        </Button>
      </DialogFooter>
    </form>
  );
}

function MintDone({ organizationSlug, minted, onDone }: { organizationSlug: string; minted: Minted; onDone: () => void }) {
  const lines = installLines(minted);
  return (
    <div className="flex flex-col gap-3">
      <DialogHeader>
        <DialogTitle className="font-mono">{minted.name}</DialogTitle>
        <DialogDescription>{minted.autoEnroll === null ? "Waits for you" : `Auto up to ${minted.autoEnroll.maxServers}`}</DialogDescription>
      </DialogHeader>
      <InstallTabs lines={lines} />
      <div className="flex flex-col gap-0.5 text-sm">
        <p className="text-muted-foreground">{minted.autoEnroll === null ? "Installs Docker and Ployz. Nothing joins until you say so." : `Installs Docker and Ployz. Up to ${minted.autoEnroll.maxServers} servers join your existing cluster when ready.`}</p>
        {minted.autoEnroll === null ? <p className="text-muted-foreground">To skip the review, run <code className="font-mono text-foreground">ployz server add --command</code> instead. It enrolls one server now.</p> : null}
        <p className="text-warning">Shown once. Expires in {cliLifetimeLabel(minted.expiresIn)}.</p>
      </div>
      <Arrivals organizationSlug={organizationSlug} token={minted.name} auto={minted.autoEnroll !== null} onDone={onDone} />
    </div>
  );
}

function Arrivals({ organizationSlug, token, auto, onDone }: { organizationSlug: string; token: string; auto: boolean; onDone: () => void }) {
  const { data } = useQuery(waitingServersQueryOptions(organizationSlug));
  const { data: tokens } = useQuery({ ...fleetTokensQueryOptions(organizationSlug), enabled: auto });
  const joined = tokens?.find((row) => row.name === token)?.joined ?? 0;
  const here = data?.filter((server) => server.fleetToken === token).length ?? 0;
  return (
    <div className="flex h-11 items-center gap-2.5 rounded-md border px-3 text-sm">
      {auto ? (
        <>
          <span className="text-muted-foreground">{joined === 0 ? "Servers join when ready" : `${joined} joined`}</span>
          <Button size="sm" variant="outline" className="ml-auto" onClick={onDone}>Show</Button>
        </>
      ) : here === 0 ? (
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
