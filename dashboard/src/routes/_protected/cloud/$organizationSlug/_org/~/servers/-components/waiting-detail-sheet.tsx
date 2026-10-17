import type { ReactNode } from "react";
import { Button } from "#/components/ui/button";
import { Sheet, SheetContent, SheetDescription, SheetFooter, SheetHeader, SheetTitle } from "#/components/ui/sheet";
import type { WaitingServerView } from "#/modules/machines/waiting/waiting-servers.server";
import { blockerLabel, bytes, lastSeen, portText, providerOf, sizeText, statusCell, volumesText } from "./waiting-row";

export function WaitingDetailSheet({ server, now, onOpenChange, onReject }: {
  server: WaitingServerView | null;
  now: number;
  onOpenChange: (open: boolean) => void;
  onReject: (id: string) => void;
}) {
  return (
    <Sheet open={server !== null} onOpenChange={onOpenChange}>
      <SheetContent className="w-full overflow-y-auto sm:max-w-md">
        {server === null ? null : <Detail server={server} now={now} onReject={onReject} />}
      </SheetContent>
    </Sheet>
  );
}

function Detail({ server, now, onReject }: { server: WaitingServerView; now: number; onReject: (id: string) => void }) {
  const { report } = server;
  const status = statusCell(server, now);
  const volumes = volumesText(report);
  return (
    <>
      <SheetHeader>
        <SheetTitle>{report.hostname}</SheetTitle>
        <SheetDescription>{status.bold} · {lastSeen(server, now)}</SheetDescription>
      </SheetHeader>
      <div className="flex flex-col gap-5 px-4">
        {report.blockers.length > 0 ? <Issues title="Blocked" issues={report.blockers} labelled /> : null}
        {report.warnings.length > 0 ? <Issues title="Warnings" issues={report.warnings} /> : null}
        <dl className="grid grid-cols-[7.5rem_minmax(0,1fr)] gap-x-3 gap-y-2">
          <Fact label="Source IP"><span className="font-mono">{server.observedIp}</span></Fact>
          <Fact label="Provider">{providerOf(server)}</Fact>
          <Fact label="Token"><span className="font-mono">{server.fleetToken}</span></Fact>
          <Fact label="OS">{report.os.id} {report.os.version} · {report.arch}</Fact>
          <Fact label="Kernel">{report.kernel}</Fact>
          <Fact label="Runs on">{report.virt}</Fact>
          <Fact label="Size">{sizeText(report)}</Fact>
          <Fact label="Disks">
            {report.disks.length === 0 ? "None" : report.disks.map((disk) => (
              <div key={disk.path} className="truncate">
                {bytes(disk.sizeBytes)} <span className="text-muted-foreground">{disk.role === "root" ? "main" : "empty"} · {disk.path}</span>
              </div>
            ))}
          </Fact>
          <Fact label="Docker">
            {report.docker.kind === "absent" ? "Not installed" : `${report.docker.version}${report.docker.compatible ? "" : " · too old"}`}
          </Fact>
          <Fact label="Volumes">{volumes.bold} <span className="text-muted-foreground">· {volumes.detail}</span></Fact>
          <Fact label="Port 80">{server.probe === null ? "Not checked yet" : portText(server.probe["80"])}</Fact>
          <Fact label="Port 443">{server.probe === null ? "Not checked yet" : portText(server.probe["443"])}</Fact>
        </dl>
      </div>
      <SheetFooter>
        <Button variant="destructive" onClick={() => onReject(server.id)}>Reject</Button>
        <p className="text-xs text-muted-foreground">It stops checking in. Asking again with the same token gets it turned away.</p>
      </SheetFooter>
    </>
  );
}

function Issues({ title, issues, labelled = false }: {
  title: string;
  issues: readonly { readonly code: string; readonly message: string }[];
  labelled?: boolean;
}) {
  return (
    <section className="flex flex-col gap-1.5">
      <h3 className="text-xs font-medium text-muted-foreground">{title}</h3>
      <ul className="flex flex-col gap-2">
        {issues.map((issue) => (
          <li key={issue.code} className="rounded-md border px-3 py-2">
            {labelled ? <p className="font-medium">{blockerLabel(issue.code)}</p> : null}
            <p className={labelled ? "text-muted-foreground" : undefined}>{issue.message}</p>
          </li>
        ))}
      </ul>
    </section>
  );
}

function Fact({ label, children }: { label: string; children: ReactNode }) {
  return (
    <>
      <dt className="text-muted-foreground">{label}</dt>
      <dd className="min-w-0">{children}</dd>
    </>
  );
}
