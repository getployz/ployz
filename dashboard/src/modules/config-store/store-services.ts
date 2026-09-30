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

/** Why a name isn't what a Store name must be (a Service's, Volume's, Environment's or Project's): a DNS label. */
export function dnsLabelError(name: string): string | null {
  if (name.length > 63) return "Use at most 63 characters.";
  return /^[a-z0-9]([a-z0-9-]*[a-z0-9])?$/u.test(name) ? null
    : "Use lowercase letters, digits and hyphens, starting and ending with a letter or digit.";
}

/** A name for something nobody named yet, like `brave-otter`: a DNS label. */
export const randomName = () => uniqueNamesGenerator({ dictionaries: [adjectives, animals], separator: "-", length: 2, style: "lowerCase" });

/**
 * A new Service's name: a DNS label from its repository or image, with a random suffix when a Service here has it as a
 * name or Private DNS. The Store refuses a name taken meanwhile.
 */
export function newServiceName(source: NewServiceSource, services: readonly ServiceListing[]) {
  return uniqueName(sourceName(source) || "service", services.flatMap((service) => [service.name, service.private_dns]));
}

const SUFFIX = "abcdefghijklmnopqrstuvwxyz0123456789";

/** `wanted` as a DNS label, given a random suffix (`postgres-x7kd`) until it is none of `taken`. */
export function uniqueName(wanted: string, taken: Iterable<string>) {
  const used = new Set(taken);
  const base = slugifySegment(wanted).slice(0, MAX_NAME).replace(/-+$/u, "") || "service";
  const stem = base.slice(0, MAX_NAME - 5).replace(/-+$/u, "");
  let name = base;
  while (used.has(name)) {
    const suffix = Array.from(crypto.getRandomValues(new Uint8Array(4)), (byte) => SUFFIX[byte % SUFFIX.length]).join("");
    name = `${stem}-${suffix}`;
  }
  return name;
}

/** The command that creates a Service from a source, with the id the caller minted. */
export function createServiceCommand(id: string, environment: EnvironmentRef, name: string, source: NewServiceSource):
  ConfigCommand & { command: "create_service" | "create_git_service" } {
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
