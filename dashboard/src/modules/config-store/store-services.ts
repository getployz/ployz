import type { ConfigCommand, DiffView, DomainRow, EnvironmentRef, EnvironmentView, JsonValue, ServiceId, ServiceListing, ServiceSettingChange } from "@ployz/sdk";
import { Option, Schema } from "effect";
import { adjectives, animals, uniqueNamesGenerator } from "unique-names-generator";
import { slugifySegment } from "#/utils/slug";

/** Where a new Service's image comes from. */
export type NewServiceSource =
  | { type: "empty" }
  | { type: "image"; image: string }
  | { type: "git"; repository: string; branch: string | null };

const MAX_NAME = 63;

function sourceName(source: NewServiceSource) {
  if (source.type === "git") return source.repository.split("/").at(-1) ?? "";
  if (source.type === "image") return source.image.split("/").at(-1)?.split(/[:@]/)[0] ?? "";
  return randomName();
}

/** What a Store name must be (a Service's, Environment's or Project's): a lowercase DNS label. */
export const isDnsLabel = (name: string) => /^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$/u.test(name);
export const DNS_LABEL_RULE = "Use lowercase letters, digits and hyphens, starting and ending with a letter or digit.";

/** A name for something nobody named yet, like `brave-otter`: a DNS label. */
export const randomName = () => uniqueNamesGenerator({ dictionaries: [adjectives, animals], separator: "-", length: 2, style: "lowerCase" });

/**
 * A new Service's name: a DNS label from its repository or image, numbered until no Service here has it as a name or
 * Private DNS. The Store refuses a name taken meanwhile.
 */
export function newServiceName(source: NewServiceSource, services: readonly ServiceListing[]) {
  const taken = new Set(services.flatMap((service) => [service.name, service.private_dns]));
  const base = slugifySegment(sourceName(source)).slice(0, MAX_NAME).replace(/-+$/u, "") || "service";
  let name = base;
  for (let n = 2; taken.has(name); n += 1) name = `${base.slice(0, MAX_NAME - String(n).length - 1).replace(/-+$/u, "")}-${n}`;
  return name;
}

/** The command that creates a Service from a source, with the id the caller minted. */
export function createServiceCommand(id: string, environment: EnvironmentRef, name: string, source: NewServiceSource): ConfigCommand {
  // SAFETY: a Service id is a UUID the caller mints; the Store checks it.
  const serviceId = id as ServiceId;
  return source.type === "git"
    ? { command: "create_git_service", id: serviceId, environment, name, repository: source.repository, branch: source.branch }
    : { command: "create_service", id: serviceId, environment, name, image: source.type === "image" ? source.image : null };
}

/** One Service's Setting rows from an Environment view, by Setting name. */
export function serviceSettingRows(view: EnvironmentView, service: string) {
  const prefix = `${service}.`;
  return new Map(view.settings.flatMap((row) => row.path.startsWith(prefix) ? [[row.path.slice(prefix.length), row] as const] : []));
}

/** What the next Deploy changes in one Service, by Setting name (`name` for a rename). */
export function serviceChanges(diff: DiffView, id: string): Map<string, ServiceSettingChange> {
  const node = diff.changes.find((change) => change.type === "service" && change.id === id);
  return new Map(node?.settings.map((row) => [row.path.slice(row.path.indexOf(".") + 1), row]) ?? []);
}

/** A scalar Setting's value as a field shows it: blank when it has none. */
export function settingText(value: JsonValue | undefined) {
  return value === null || value === undefined ? "" : String(value);
}

/** A route row's value in a diff, as far as a domain needs it. */
const decodeRoute = Schema.decodeUnknownOption(Schema.Struct({ hostname: Schema.String }));

/**
 * Whether the next Deploy changes a domain: a generated one with its Service's `managedHostnames`, a custom one with
 * the route row whose value before or after has its hostname.
 */
export function domainChanged(changes: Map<string, ServiceSettingChange>, domain: DomainRow) {
  if (domain.kind === "generated") return changes.has("managedHostnames");
  const hasHostname = (route: JsonValue) => Option.exists(decodeRoute(route), ({ hostname }) => hostname === domain.hostname);
  return [...changes].some(([setting, change]) => setting.startsWith("routes.") && (hasHostname(change.before) || hasHostname(change.after)));
}
