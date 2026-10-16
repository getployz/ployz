import { useState } from "react";
import { ZapIcon } from "lucide-react";
import type { DomainRow, JsonValue, ServiceSettingChange } from "@ployz/sdk";
import { Link } from "@tanstack/react-router";
import { PLATFORM_HTTP_PORT } from "#/modules/variables/managed-service-exports";
import { changedProps, settingText } from "#/modules/config-store/store-services";
import { useStorePendingDomains, useStorePendingNodes } from "#/modules/config-store/store-pending";
import type { StoreService } from "./StoreServiceDrawer";
import { ServiceSettingInput } from "./ServiceSettingInput";
import { SettingsAddButton, SettingsFoot, SettingsRow } from "#/components/settings/settings-section";
import { SettingsEmpty, SettingsList, SettingsListItem } from "#/components/settings/settings-list";
import { Schema } from "effect";
import { domainChanged } from "#/modules/config-store/store-services";
import { domainsQuery, requireView, useStoreView } from "#/modules/config-store/store-view.queries";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { CustomDomainDialog, type CustomDomain } from "./CustomDomainDialog";
import { DomainTitle, PendingLabel, PublicDomainRow, storeStatusView } from "./domain-row";
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
export function StoreNetworkingSection({ state, privateFirst = false, validatePrivateDns }: {
  state: StoreService;
  /** A database is reached privately: its address leads, and domains follow. */
  privateFirst?: boolean;
  validatePrivateDns: (raw: string) => string | null;
}) {
  const { organizationSlug, environment, service, changes } = state;
  const privateDns = settingText(state.rows.get("privateDns")?.value) || service.private_dns;
  const all = requireView(useStoreView(organizationSlug, domainsQuery(environment))).domains;
  const leaving = useStorePendingNodes(organizationSlug, environment).leaving.domain;
  const domains = all.filter((domain) => domain.service === service.name
    && !leaving.has(nameOf(domain)) && !(domain.hostname !== null && leaving.has(domain.hostname)));
  const pendingNames = useStorePendingDomains(organizationSlug, environment, service.name);
  const names = (domain: DomainRow) => [nameOf(domain), domain.hostname];
  const isPending = (domain: DomainRow) => names(domain).some((name) => name !== null && pendingNames.includes(name));
  // A write the listed rows don't name is a domain still being added; it shows its literal name until the Store answers.
  const adding = pendingNames.filter((name) => name === null || !domains.some((domain) => names(domain).includes(name)));
  const writer = useStoreWriter(organizationSlug);
  const [editor, setEditor] = useState<Editor>(null);
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
  const none = domains.length === 0 && removed.length === 0 && adding.length === 0;
  // A domain without its own port routes to PORT, which Ployz sets when the user doesn't. Whether the app listens
  // there is unknown, so this is a hint, never a failure.
  const defaultPort = defaultPortHint(state.rows.get("env.PORT")?.value, domains);
  const privateRow = (
    <SettingsRow label="Private address" hint="How other services in this environment reach it." commit="confirm" control={
      <ServiceSettingInput ariaLabel="Private address" suffix=".internal" placeholder={service.name} value={privateDns}
        {...changedProps(changes.get("privateDns"))}
        validate={validatePrivateDns}
        onCommit={(raw) => state.set("privateDns", raw === "" ? null : raw)} />
    } />
  );

  return (
    <>
      {privateFirst ? privateRow : null}
      {none ? <SettingsEmpty title="No public domains">Generate one under the cluster domain, or add your own.</SettingsEmpty> : (
        <SettingsList label="Public domains">
          {removed.map((removed) => (
            <SettingsListItem key={`removed:${removed.name}`} media={<ZapIcon />} label={removed.name} tone="removed"
              hint="Removed on your next deploy." commit="none" />
          ))}
          {domains.map((domain) => {
            const name = nameOf(domain);
            const hostname = domain.hostname;
            return (
              <PublicDomainRow
                key={`${domain.kind}:${name}`}
                organizationSlug={organizationSlug}
                title={hostname ? <DomainTitle hostname={hostname} live={domain.status === "ready"} /> : (
                  <span className="font-mono">{name}<span className="text-muted-foreground">.…</span></span>
                )}
                label={hostname ?? name}
                portLabel={portLabel(domain.port)}
                view={storeStatusView(domain)}
                dnsRecords={domain.action?.type === "dns" ? domain.action.records : []}
                changed={domainChanged(changes, domain)}
                pending={isPending(domain)}
                onEdit={() => domain.kind === "custom"
                  ? setEditor({ kind: "custom", hostname: domain.hostname })
                  : setEditor({ kind: "generated", prefix: domain.prefix })}
                onDelete={() => remove(name)}
              />
            );
          })}
          {adding.map((name, index) => (
            <SettingsListItem key={`adding:${name ?? index}`} media={<ZapIcon className="opacity-50" />}
              label={<span aria-busy className="font-mono opacity-60">{name ?? "New domain"}</span>}
              commit="none" actions={<PendingLabel>Adding…</PendingLabel>} />
          ))}
        </SettingsList>
      )}
      <SettingsFoot>
        <SettingsAddButton onClick={() => setEditor({ kind: "generate" })}>Generate domain</SettingsAddButton>
        <SettingsAddButton onClick={() => setEditor({ kind: "add" })}>Custom domain</SettingsAddButton>
      </SettingsFoot>
      {defaultPort ? (
        <SettingsRow label="Default port" commit="none"
          hint={<>Domains without a port use {defaultPort}. <Link to="." search={(prev) => ({ ...prev, tab: "variables" })} className="underline">Change</Link></>} />
      ) : null}
      {privateFirst ? null : privateRow}
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
            const next = prefix.trim().toLowerCase();
            void writer.commit({
              command: "set_generated_domain", environment, service: service.name, domain: generated.prefix, prefix: next, port: targetPort,
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
            add(hostname, targetPort).isPersisted.promise.catch(() => {
              if (!edited) setEditor({ kind: "add", draft: { hostname, targetPort } });
            });
          }}
        />
      ) : null}
    </>
  );
}
