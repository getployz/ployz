import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { NetworkIcon, PencilIcon, PlusIcon, ZapIcon } from "lucide-react";
import type { DomainRow, ServiceSettingChange } from "@ployz/sdk";
import { Button } from "#/components/ui/button";
import { customDomainCapabilityQueryOptions } from "#/modules/billing/billing.queries";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "#/components/ui/dialog";
import { changedProps, settingText } from "#/modules/config-store/store-services";
import type { StoreService } from "./StoreServiceDrawer";
import { ServiceSettingInput } from "./ServiceSettingInput";
import { Empty, EmptyDescription } from "#/components/ui/empty";
import { Field, FieldDescription, FieldGroup, FieldLabel } from "#/components/ui/field";
import { Schema } from "effect";
import { domainChanged } from "#/modules/config-store/store-services";
import { domainsQuery, requireView, useStoreView } from "#/modules/config-store/store-view.queries";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { CustomDomainDialog, type CustomDomain } from "./CustomDomainDialog";
import { CustomDomainUpsellSheet } from "./CustomDomainUpsellSheet";
import { DomainRowShell, DomainTitle, PublicDomainRow, storeStatusView } from "./domain-row";
import { ManagedDomainDialog } from "./ManagedDomain";

type CustomDomainEditor = { kind: "add"; draft?: CustomDomain } | { kind: "custom"; hostname: string };
/** `upsell` pitches Pro, then opens the custom domain editor the user asked for. */
type Editor = { kind: "generate" } | { kind: "generated" } | CustomDomainEditor | { kind: "upsell"; then: CustomDomainEditor } | null;

/** How a domain is addressed in commands: its hostname, or a generated one's prefix. */
const nameOf = (domain: DomainRow) => domain.kind === "custom" ? domain.hostname : domain.prefix;

const portLabel = (port: number | null) => port === null ? "Uses PORT" : `Port ${port}`;

const Route = Schema.Struct({ hostname: Schema.String });
const Managed = Schema.Array(Schema.Struct({ prefix: Schema.String }));

/**
 * Domains the next Deploy removes, from the review: a custom one's route row going, or the generated one when its
 * Service keeps none. They stay listed, struck through, until that Deploy.
 */
function removedDomains(changes: Map<string, ServiceSettingChange>, domains: readonly DomainRow[]) {
  return [...changes.values()].flatMap((change) => {
    const setting = change.path.slice(change.path.indexOf(".") + 1);
    if (setting.startsWith("routes.") && change.after === null && Schema.is(Route)(change.before)) {
      return [{ name: change.before.hostname, path: change.path }];
    }
    const kept = Schema.is(Managed)(change.after) ? change.after : [];
    if (setting === "managedHostnames" && Schema.is(Managed)(change.before) && kept.length === 0
      && !domains.some((domain) => domain.kind === "generated")) {
      return change.before.map(({ prefix }) => ({ name: prefix, path: change.path }));
    }
    return [];
  });
}

/**
 * A Service's domains over the Config Store: at most one generated domain under the Cluster Domain and any custom
 * ones, each with the status the Store gives it. Adding, retargeting and removing one are staged for the next Deploy.
 */
export function StoreNetworkingSection({ state, version, validatePrivateDns }: {
  state: StoreService;
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
  const queryClient = useQueryClient();
  const capabilityQuery = customDomainCapabilityQueryOptions(organizationSlug);
  // Unknown yet reads as allowed: the Store still refuses, and a paying Organization never waits on this.
  const needsPro = useQuery(capabilityQuery).data === false;
  const openCustomDomain = (next: CustomDomainEditor) => setEditor(needsPro ? { kind: "upsell", then: next } : next);
  const [editingPrivateDns, setEditingPrivateDns] = useState(false);
  const privateDnsChange = changes.get("privateDns");
  const generated = domains.find((domain) => domain.kind === "generated");
  const edited = editor?.kind === "custom"
    ? domains.find((domain) => domain.kind === "custom" && domain.hostname === editor.hostname)
    : undefined;
  const add = (hostname: string | null, port: number | null) =>
    writer.commit({ command: "add_domain", environment, service: service.name, hostname, port });
  const remove = (domain: string) => writer.commit({ command: "remove_domain", environment, domain });

  return (
    <FieldGroup>
      <Field data-changed={domains.some((domain) => domainChanged(changes, domain)) || undefined}>
        <FieldLabel>Public Networking</FieldLabel>
        <FieldDescription>Access your application over HTTP with the following domains.</FieldDescription>
        <div className="flex flex-col gap-2">
          {removedDomains(changes, domains).map((removed) => (
            <DomainRowShell key={`removed:${removed.name}`} icon={<ZapIcon className="opacity-50" />} changed actions={(
              <Button type="button" variant="ghost" size="sm"
                onClick={() => writer.commit({ command: "discard", environment, path: removed.path, version })}>Undo</Button>
            )}>
              <div className="truncate font-mono text-sm text-muted-foreground line-through">{removed.name}</div>
              <div className="truncate text-muted-foreground text-sm">Removed on your next deploy</div>
            </DomainRowShell>
          ))}
          {domains.length === 0 && removedDomains(changes, domains).length === 0 ? (
            <Empty>
              <EmptyDescription>No public domains yet.</EmptyDescription>
            </Empty>
          ) : null}
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
                  ? openCustomDomain({ kind: "custom", hostname: domain.hostname })
                  : setEditor({ kind: "generated" })}
                onDelete={() => remove(name)}
              />
            );
          })}
        </div>
        <div className="flex flex-wrap gap-2">
          {generated ? null : (
            <Button type="button" variant="outline" onClick={() => setEditor({ kind: "generate" })}>
              <ZapIcon data-icon="inline-start" />
              Generate Domain
            </Button>
          )}
          <Button type="button" variant="outline" onClick={() => openCustomDomain({ kind: "add" })}>
            <PlusIcon data-icon="inline-start" />
            Custom Domain
          </Button>
        </div>
        {editor?.kind === "generate" || (editor?.kind === "generated" && generated?.kind === "generated") ? (
          <ManagedDomainDialog
            mode={editor.kind === "generate" ? "generate" : "edit"}
            managed={{ prefix: generated?.kind === "generated" ? generated.prefix : service.private_dns, targetPort: generated?.port ?? null }}
            clusterDomain={generated?.kind === "generated" && generated.hostname ? generated.hostname.slice(generated.prefix.length + 1) : null}
            // The Environment's other generated domains; the Store checks the whole Organization and what Servers publish.
            takenPrefixes={all.flatMap((domain) => domain.kind === "generated" && domain.service !== service.name ? [domain.prefix] : [])}
            defaultTargetPort={null}
            onClose={() => setEditor(null)}
            onSubmit={({ prefix, targetPort }) => {
              if (generated?.kind !== "generated") return void add(null, targetPort);
              // One command: the subdomain, and the port (null follows the container's PORT).
              writer.commit({ command: "set_generated_domain", environment, service: service.name, prefix: prefix.trim().toLowerCase(), port: targetPort });
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
              // Shown at once; a refusal toasts, then offers Pro if that was the reason, else reopens with what was typed.
              add(hostname, targetPort).isPersisted.promise.catch(async () => {
                const allowed = await queryClient.fetchQuery({ ...capabilityQuery, staleTime: 0 }).catch(() => true);
                const retry: CustomDomainEditor = edited ? { kind: "custom", hostname } : { kind: "add", draft: { hostname, targetPort } };
                if (!allowed) setEditor({ kind: "upsell", then: retry });
                else if (!edited) setEditor(retry);
              });
            }}
          />
        ) : null}
        {editor?.kind === "upsell" ? (
          <CustomDomainUpsellSheet
            organizationSlug={organizationSlug}
            service={service.name}
            environment={environment.environment ?? null}
            onUpgraded={() => setEditor(editor.then)}
            onClose={() => setEditor(null)}
          />
        ) : null}
      </Field>
      <Field data-changed={privateDnsChange ? true : undefined}>
        <FieldLabel>Private Networking</FieldLabel>
        <FieldDescription>Communicate with this service from within the environment.</FieldDescription>
        <DomainRowShell icon={<NetworkIcon />} changed={privateDnsChange !== undefined} actions={(
          <Button type="button" variant="ghost" size="icon-sm" aria-label="Edit private endpoint" onClick={() => setEditingPrivateDns(true)}>
            <PencilIcon />
          </Button>
        )}>
          <DomainTitle hostname={`${privateDns}.internal`} copyLabel="Copy private hostname" />
          <div className="truncate text-muted-foreground text-sm">
            → or just <span className="font-mono">{privateDns}</span>
          </div>
        </DomainRowShell>
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
    </FieldGroup>
  );
}
