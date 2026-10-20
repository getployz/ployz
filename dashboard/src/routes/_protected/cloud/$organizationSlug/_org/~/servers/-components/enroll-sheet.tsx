import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { useEffect, useState } from "react";
import { toast } from "sonner";
import { Alert, AlertDescription, AlertTitle } from "#/components/ui/alert";
import { Button } from "#/components/ui/button";
import { Checkbox } from "#/components/ui/checkbox";
import { Field, FieldDescription, FieldGroup, FieldLabel, FieldLegend, FieldSet } from "#/components/ui/field";
import { Input } from "#/components/ui/input";
import { NativeSelect, NativeSelectOption } from "#/components/ui/native-select";
import { Sheet, SheetContent, SheetDescription, SheetFooter, SheetHeader, SheetTitle } from "#/components/ui/sheet";
import { Skeleton } from "#/components/ui/skeleton";
import { toErrorMessage } from "#/lib/error-message";
import type { EnrollInput } from "#/modules/machines/waiting/server-enrollment";
import type { WaitingServerView } from "#/modules/machines/waiting/waiting-servers.server";
import { startServerEnrollmentServerFn } from "#/modules/machines/waiting/waiting.functions";
import { enrollmentPlanQueryOptions, fleetTokenKeys, serverEnrollmentsQueryOptions, waitingServersQueryOptions } from "#/modules/machines/waiting/waiting.queries";

export function EnrollSheet({ organizationSlug, servers, hasFirst, serversOffline, onClose }: {
  organizationSlug: string;
  servers: readonly WaitingServerView[];
  hasFirst: boolean;
  /** None of the Organization's Servers answered Cloud, so it refuses the enroll until they're back or forgotten. */
  serversOffline: boolean;
  onClose: () => void;
}) {
  const [initialHasFirst] = useState(hasFirst);
  const [first, setFirst] = useState(servers[0]?.id ?? "");
  const [storage, setStorage] = useState("zfs");
  const [fallback, setFallback] = useState(true);
  const [names, setNames] = useState("hostnames");
  const [pattern, setPattern] = useState("web-{n}");
  const [settledPattern, setSettledPattern] = useState(pattern);
  useEffect(() => {
    const timer = setTimeout(() => setSettledPattern(pattern), 300);
    return () => clearTimeout(timer);
  }, [pattern]);
  const [runs, setRuns] = useState({ services: true, ingress: true, builds: true });
  const choices = {
    ids: servers.map((row) => row.id),
    storage: storage === "none" ? { kind: "none" } : { kind: "zfs", useSpareDisk: false, fallbackToDocker: fallback },
    names: names === "pattern" ? { kind: "pattern", pattern: settledPattern } : { kind: "hostnames" },
    runs,
  } satisfies EnrollInput;
  const input: EnrollInput = hasFirst ? choices : { ...choices, first };
  const plan = useQuery(enrollmentPlanQueryOptions(organizationSlug, input));
  const queryClient = useQueryClient();
  const start = useMutation({
    mutationFn: () => startServerEnrollmentServerFn({ data: { organizationSlug, ...input } }),
    onSuccess: (result) => {
      toast.success(`Enrolling ${result.attempts.length} ${result.attempts.length === 1 ? "server" : "servers"}${result.skipped.length ? ` · ${result.skipped.length} skipped` : ""}`);
      onClose();
    },
    onError: (cause) => toast.error(toErrorMessage(cause, "Couldn't enroll servers.")),
    onSettled: () => Promise.all([
      queryClient.invalidateQueries({ queryKey: waitingServersQueryOptions(organizationSlug).queryKey }),
      queryClient.invalidateQueries({ queryKey: serverEnrollmentsQueryOptions(organizationSlug).queryKey }),
      queryClient.invalidateQueries({ queryKey: fleetTokenKeys.all }),
    ]),
  });
  const preview = plan.data;
  const count = preview?.rows.length ?? 0;
  return <Sheet open onOpenChange={(open) => { if (!open) onClose(); }}>
    <SheetContent className="w-full sm:max-w-md">
      <SheetHeader>
        <SheetTitle>Enroll {servers.length} {servers.length === 1 ? "server" : "servers"}</SheetTitle>
        <SheetDescription>{hasFirst ? "Join your existing cluster." : servers.length === 1 ? `${servers[0]?.report.hostname} starts your cluster.` : "One goes first. The rest follow."}</SheetDescription>
      </SheetHeader>
      <div className="flex-1 overflow-y-auto px-4">
        <FieldGroup>
          {initialHasFirst !== hasFirst ? <FieldDescription>{hasFirst ? "Another server is starting the cluster." : "Choose the first server again."}</FieldDescription> : null}
          {serversOffline ? <Alert variant="destructive">
            <AlertTitle>Servers offline</AlertTitle>
            <AlertDescription>Forget them first. <Link to="/cloud/$organizationSlug/~/settings" params={{ organizationSlug }} hash="organization-teardown" className="underline underline-offset-4">Forget Servers</Link></AlertDescription>
          </Alert> : null}
          {preview && (preview.spread.providers > 1 || preview.spread.networks > 3) ? <Alert>
            <AlertTitle>{preview.spread.networks} networks, {preview.spread.providers} providers</AlertTitle>
            <AlertDescription>Make sure they’re all yours.<details><summary>Show sources</summary>{servers.map((row) => <p key={row.id}>{row.report.hostname} · {row.observedIp} · {row.report.provider ?? "Unknown"}</p>)}</details></AlertDescription>
          </Alert> : null}
          {!hasFirst && servers.length > 1 ? <Field>
            <FieldLabel htmlFor="enroll-first">First server</FieldLabel>
            <NativeSelect id="enroll-first" value={first} onChange={(event) => setFirst(event.target.value)}>
              {servers.map((row) => <NativeSelectOption key={row.id} value={row.id}>{row.report.hostname} · {row.observedIp}</NativeSelectOption>)}
            </NativeSelect>
            <FieldDescription>Everything else joins once this one’s up.</FieldDescription>
          </Field> : null}
          <FieldSet>
            <FieldLegend>Volumes</FieldLegend>
            <Field><NativeSelect aria-label="Volumes" value={storage} onChange={(event) => setStorage(event.target.value)}>
              <NativeSelectOption value="zfs">ZFS · Volumes get size limits</NativeSelectOption>
              <NativeSelectOption value="none">Docker only · No size limits</NativeSelectOption>
            </NativeSelect></Field>
            {storage === "zfs" ? <>
              <FieldDescription>ZFS uses the main disk.</FieldDescription>
              <Field orientation="horizontal"><Checkbox id="enroll-fallback" checked={fallback} onCheckedChange={setFallback} /><FieldLabel htmlFor="enroll-fallback">Fall back to Docker where ZFS can’t run</FieldLabel></Field>
            </> : null}
          </FieldSet>
          <FieldSet>
            <FieldLegend>Names</FieldLegend>
            <Field><NativeSelect aria-label="Names" value={names} onChange={(event) => setNames(event.target.value)}>
              <NativeSelectOption value="hostnames">Hostnames</NativeSelectOption><NativeSelectOption value="pattern">Pattern</NativeSelectOption>
            </NativeSelect></Field>
            {names === "pattern" ? <Field><FieldLabel htmlFor="enroll-pattern">Name pattern</FieldLabel><Input id="enroll-pattern" value={pattern} onChange={(event) => setPattern(event.target.value)} /><FieldDescription>Use {"{n}"} for consecutive numbers.</FieldDescription></Field> : null}
            <FieldDescription>{preview?.rows.slice(0, 3).map((row) => row.instruction.name).join(", ")}{count > 3 ? ", …" : ""}</FieldDescription>
          </FieldSet>
          <FieldSet><FieldLegend>Runs</FieldLegend>
            <Field orientation="horizontal"><Checkbox id="enroll-services" checked={runs.services} onCheckedChange={(services) => setRuns({ ...runs, services })} /><FieldLabel htmlFor="enroll-services">Services</FieldLabel></Field>
            <Field orientation="horizontal"><Checkbox id="enroll-ingress" checked={runs.ingress} onCheckedChange={(ingress) => setRuns({ ...runs, ingress })} /><FieldLabel htmlFor="enroll-ingress">Web traffic</FieldLabel></Field>
            <Field orientation="horizontal"><Checkbox id="enroll-builds" checked={runs.builds} onCheckedChange={(builds) => setRuns({ ...runs, builds })} /><FieldLabel htmlFor="enroll-builds">Builds</FieldLabel></Field>
          </FieldSet>
          {plan.isPending ? <Skeleton className="h-16" /> : plan.isError ? <Alert variant="destructive"><AlertDescription>{toErrorMessage(plan.error, "Couldn't plan enrollment.")}</AlertDescription></Alert> : preview ? <section aria-label="Enrollment preview" className="flex flex-col gap-2 text-xs">
            {preview.rows.map((row) => <p key={row.waitingServerId} className="flex justify-between gap-3"><span className="truncate">{row.instruction.name}</span><span className="shrink-0 text-muted-foreground">{row.instruction.storage.kind === "none" ? "Docker only" : row.instruction.storage.device ? "ZFS on spare disk" : "ZFS on main disk"}</span></p>)}
            {preview.skipped.map((row) => <p key={row.id}>Skip · {servers.find((host) => host.id === row.id)?.report.hostname ?? row.id} · {row.reason}</p>)}
          </section> : null}
        </FieldGroup>
      </div>
      <SheetFooter>
        <p className="text-xs text-muted-foreground">{hasFirst ? "Joins 8 at a time." : "First one, then 8 at a time."}</p>
        <Button disabled={start.isPending || plan.isPending || plan.isPlaceholderData || plan.isError || (names === "pattern" && pattern !== settledPattern) || count === 0} onClick={() => start.mutate()}>Enroll {count}</Button>
        <Button variant="outline" onClick={onClose}>Cancel</Button>
      </SheetFooter>
    </SheetContent>
  </Sheet>;
}
