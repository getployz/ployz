import { useState } from "react";
import { PencilIcon, PlusIcon, ZapIcon } from "lucide-react";
import type { DomainRow, JsonValue, ServiceSettingChange } from "@ployz/sdk";
import { Link } from "@tanstack/react-router";
import { PLATFORM_HTTP_PORT } from "#/modules/variables/managed-service-exports";
import { Button } from "#/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "#/components/ui/dialog";
import { changedProps, settingText } from "#/modules/config-store/store-services";
import type { StoreService } from "./StoreServiceDrawer";
import { ServiceSettingInput } from "./ServiceSettingInput";
import { Field, FieldContent, FieldDescription, FieldLabel } from "#/components/ui/field";
import { cn } from "#/lib/utils";
import { Schema } from "effect";
import { domainChanged } from "#/modules/config-store/store-services";
import { domainsQuery, requireView, useStoreView } from "#/modules/config-store/store-view.queries";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { CustomDomainDialog, type CustomDomain } from "./CustomDomainDialog";
import { DomainRowShell, DomainTitle, PublicDomainRow, storeStatusView } from "./domain-row";
import { ManagedDomainDialog } from "./ManagedDomain";

type Editor =
  | { kind: "generate" }
  | { kind: "generated"; prefix: string }
  | { kind: "add"; draft?: CustomDomain }
  | { kind: "custom"; hostname: string }
  | null;

/** How a domain is addressed in commands: its hostname, or a generated one's prefix. */
const nameOf = (domain: DomainRow) => domain.kind === "custom" ? domain.hostname : domain.prefix;

const prefixesOf = (domains: readonly DomainRow[]) =>
  domains.flatMap((domain) => domain.kind === "generated" ? [domain.prefix] : []);

const portLabel = (port: number | null) => port === null ? "Uses PORT" : `Port ${port}`;

const Route = Schema.Struct({ hostname: Schema.String });
const Managed = Schema.Array(Schema.Struct({ prefix: Schema.String }));

/** `base`, else the first of `base-2`, `base-3`, … not taken, as the Store picks one; the Store still checks it. */
export function freePrefix(base: string, taken: readonly string[]) {
  let candidate = base;
  for (let n = 2; taken.includes(candidate); n += 1) {
    const suffix = `-${n}`;
    candidate = `${base.slice(0, 63 - suffix.length).replace(/-+$/u, "")}${suffix}`;
  }
  return candidate;
}

/**
 * Domains the next Deploy removes, from the review: a custom one's route row going, or generated ones when its
 * Service keeps fewer. They stay listed, struck through, until that Deploy.
 */
function removedDomains(changes: Map<string, ServiceSettingChange>, domains: readonly DomainRow[]) {
  return [...changes.values()].flatMap((change) => {
    const setting = change.path.slice(change.path.indexOf(".") + 1);
    if (setting.startsWith("routes.") && change.after === null && Schema.is(Route)(change.before)) {
      return [{ name: change.before.hostname, path: change.path }];
    }
    const kept = Schema.is(Managed)(change.after) ? change.after : [];
    if (setting !== "managedHostnames" || !Schema.is(Managed)(change.before) || kept.length >= change.before.length) {
      return [];
    }
    // A renamed one isn't removed: only fewer generated domains show as removals.
    const listed = (prefix: string) => kept.some((managed) => managed.prefix === prefix)
      || domains.some((domain) => domain.kind === "generated" && domain.prefix === prefix);
    return change.before.filter(({ prefix }) => !listed(prefix)).map(({ prefix }) => ({ name: prefix, path: change.path }));
  });
}

/** The default port a domain routes to, when one routes to PORT and the user set none; else null. */
export function defaultPortHint(port: JsonValue | undefined, domains: readonly Pick<DomainRow, "port">[]) {
  return settingText(port) === "" && domains.some((domain) => domain.port === null) ? PLATFORM_HTTP_PORT : null;
}

/**
 * A Service's domains over the Config Store: generated ones under the Cluster Domain and custom ones, each with the
 * status the Store gives it. Adding, retargeting and removing one are staged for the next Deploy.
 */
export function StoreNetworkingSection({ state, version, privateFirst = false, validatePrivateDns }: {
  state: StoreService;
  /** A database is reached privately: its address leads, and domains follow. */
  privateFirst?: boolean;
  /** The review's version, which undoing a removal discards against. */
  version: string;
  validatePrivateDns: (raw: string) => string | null;
}) {
  const { organizationSlug, environment, service, changes } = state;
  // Its Private DNS name, with a pending edit.
  const privateDns = settingText(state.rows.get("privateDns")?.value) || service.private_dns;
  const all = requireView(useStoreView(organizationSlug, domainsQuery(environment))).domains;
  const domains = all.filter((domain) => domain.service === service.name);
  const writer = useStoreWriter(organizationSlug);
  const [editor, setEditor] = useState<Editor>(null);
  const [editingPrivateDns, setEditingPrivateDns] = useState(false);
  const privateDnsChange = changes.get("privateDns");
  const own = prefixesOf(domains);
  const generated = editor?.kind === "generated"
    ? domains.find((domain) => domain.kind === "generated" && domain.prefix === editor.prefix)
    : undefined;
  const edited = editor?.kind === "custom"
    ? domains.find((domain) => domain.kind === "custom" && domain.hostname === editor.hostname)
    : undefined;
  // The Environment's generated domains; the Store checks the whole Organization and what Servers publish.
  const allPrefixes = prefixesOf(all);
  const clusterDomain = all.flatMap((domain) =>
    domain.kind === "generated" && domain.hostname ? [domain.hostname.slice(domain.prefix.length + 1)] : [])[0] ?? null;
  const add = (hostname: string | null, port: number | null, prefix: string | null = null) =>
    writer.commit({ command: "add_domain", environment, service: service.name, hostname, prefix, port });
  const remove = (domain: string) => writer.commit({ command: "remove_domain", environment, domain });

  const removed = removedDomains(changes, domains);
  const addButtons = (
    <div className="flex shrink-0 flex-wrap gap-2">
      <Button type="button" variant="outline" onClick={() => setEditor({ kind: "generate" })}>
        <ZapIcon data-icon="inline-start" />
        Generate domain
      </Button>
      <Button type="button" variant="outline" onClick={() => setEditor({ kind: "add" })}>
        <PlusIcon data-icon="inline-start" />
        Custom domain
      </Button>
    </div>
  );
  const none = domains.length === 0 && removed.length === 0;
  // A domain without its own port routes to PORT, which Ployz sets when the user doesn't. Whether the app listens
  // there is unknown, so this is a hint, never a failure.
  const defaultPort = defaultPortHint(state.rows.get("env.PORT")?.value, domains);
  const privateRow = (
      <Field orientation="responsive" data-changed={privateDnsChange ? true : undefined}>
        <FieldContent>
          <FieldLabel>Private address</FieldLabel>
        </FieldContent>
        <div className={cn("flex min-w-0 shrink-0 items-center gap-1", privateDnsChange && "rounded-lg border border-changed-border bg-changed-soft px-2")}
          title={privateDnsChange ? `Deployed: ${settingText(privateDnsChange.before) || service.name}` : undefined}>
          <DomainTitle hostname={`${privateDns}.internal`} copyLabel="Copy private address" />
          <Button type="button" variant="ghost" size="icon-xs" aria-label="Edit private address" onClick={() => setEditingPrivateDns(true)}>
            <PencilIcon />
          </Button>
        </div>
        {editingPrivateDns ? (
          <Dialog open onOpenChange={(open) => { if (!open) setEditingPrivateDns(false); }}>
            <DialogContent>
              <DialogHeader>
                <DialogTitle>Edit private endpoint</DialogTitle>
                <DialogDescription>The name other services in this environment use to reach it. Blank returns it to the service's name.</DialogDescription>
              </DialogHeader>
              <ServiceSettingInput ariaLabel="Private endpoint name" placeholder={service.name} value={privateDns}
                {...changedProps(privateDnsChange)}
                validate={validatePrivateDns}
                onCommit={(raw) => {
                  setEditingPrivateDns(false);
                  return state.set("privateDns", raw === "" ? null : raw);
                }} />
            </DialogContent>
          </Dialog>
        ) : null}
      </Field>
  );

  return (
    <>
      {privateFirst ? privateRow : null}
      <Field orientation="responsive" data-changed={domains.some((domain) => domainChanged(changes, domain)) || undefined}>
        <FieldContent>
          <FieldLabel>Public domains</FieldLabel>
          {none ? <FieldDescription>None</FieldDescription> : null}
        </FieldContent>
        {addButtons}
      </Field>
      {none ? null : (
        <div className="flex flex-col gap-1">
          {removed.map((removed) => (
            <DomainRowShell key={`removed:${removed.name}`} icon={<ZapIcon className="opacity-50" />} changed actions={(
              <Button type="button" variant="ghost" size="sm"
                onClick={() => writer.commit({ command: "discard", environment, path: removed.path, version })}>Undo</Button>
            )}>
              <div className="truncate font-mono text-sm text-muted-foreground line-through">{removed.name}</div>
              <div className="truncate text-muted-foreground text-sm">Removed on your next deploy</div>
            </DomainRowShell>
          ))}
          {domains.map((domain) => {
            const name = nameOf(domain);
            const hostname = domain.hostname;
            return (
              <PublicDomainRow
                key={`${domain.kind}:${name}`}
                organizationSlug={organizationSlug}
                title={hostname ? <DomainTitle hostname={hostname} live={domain.status === "ready"} /> : (
                  <div className="truncate font-mono text-sm">
                    {name}
                    <span className="text-muted-foreground">.…</span>
                  </div>
                )}
                label={hostname ?? name}
                portLabel={portLabel(domain.port)}
                view={storeStatusView(domain)}
                dnsRecords={domain.action?.type === "dns" ? domain.action.records : []}
                changed={domainChanged(changes, domain)}
                onEdit={() => domain.kind === "custom"
                  ? setEditor({ kind: "custom", hostname: domain.hostname })
                  : setEditor({ kind: "generated", prefix: domain.prefix })}
                onDelete={() => remove(name)}
              />
            );
          })}
        </div>
      )}
      {defaultPort ? (
        <FieldDescription>
          Uses port {defaultPort}. <Link to="." search={(prev) => ({ ...prev, tab: "variables" })}>Change</Link>
        </FieldDescription>
      ) : null}
        {editor?.kind === "generate" ? (
          <ManagedDomainDialog
            mode="generate"
            // The first one takes the Private DNS name; another needs a subdomain of its own.
            choosePrefix={own.length > 0}
            managed={{ prefix: freePrefix(privateDns, allPrefixes), targetPort: null }}
            clusterDomain={clusterDomain}
            takenPrefixes={allPrefixes}
            defaultTargetPort={null}
            onClose={() => setEditor(null)}
            onSubmit={({ prefix, targetPort }) => void add(null, targetPort, own.length > 0 ? prefix.trim().toLowerCase() : null)}
          />
        ) : null}
        {generated?.kind === "generated" ? (
          <ManagedDomainDialog
            managed={{ prefix: generated.prefix, targetPort: generated.port }}
            clusterDomain={clusterDomain}
            takenPrefixes={allPrefixes.filter((prefix) => prefix !== generated.prefix)}
            defaultTargetPort={null}
            onClose={() => setEditor(null)}
            onSubmit={({ prefix, targetPort }) => {
              // One command: the subdomain, and the port (null follows the container's PORT).
              writer.commit({
                command: "set_generated_domain", environment, service: service.name, domain: generated.prefix,
                prefix: prefix.trim().toLowerCase(), port: targetPort,
              });
            }}
          />
        ) : null}
        {editor?.kind === "add" || (editor?.kind === "custom" && edited?.kind === "custom") ? (
          <CustomDomainDialog
            route={edited?.kind === "custom" ? { hostname: edited.hostname, targetPort: edited.port } : undefined}
            hostnameFixed
            defaultTargetPort={null}
            onClose={() => setEditor(null)}
            initial={editor.kind === "add" ? editor.draft : undefined}
            onSubmit={({ hostname, targetPort }) => {
              // Shown at once; a refusal toasts, then reopens with what was typed.
              add(hostname, targetPort).isPersisted.promise.catch(() => {
                if (!edited) setEditor({ kind: "add", draft: { hostname, targetPort } });
              });
            }}
          />
        ) : null}
      {privateFirst ? null : privateRow}
    </>
  );
}
