import type { EnvironmentView, ServiceListing } from "@ployz/sdk";
import type { ServiceSettingName } from "./catalog";
import { randomText, serviceSettingRows, settingText } from "./store-services";

/** Option values by key, as a rule reads them; null while a required one is unset. */
export type Values = Readonly<Record<string, string | null>>;

/** One input of an Install: a value, not set yet (a required one blocks Deploy), or two values that met in a Sync. */
export type Input =
  | { state: "value"; value: string }
  | { state: "unset" }
  | { state: "conflict"; here: string; theirs: string };

/**
 * One use of a template in an Environment: its release and inputs, and nothing else authored. Its members are
 * ordinary Services stamped with the template; the Install's Receipt names which.
 */
export type Install = {
  name: string;
  template: string;
  version: number;
  inputs: Readonly<Record<string, Input>>;
  /** Every direct member, and its Volumes, on its own server. Postgres HA sets it. */
  distinctMachines: boolean;
};

/**
 * The generated base of an Install: what its release rendered, and the values minted once. An update merges against
 * it and never renders the old release again; it never takes in edits.
 */
export type Receipt = { version: number; members: TemplateMember[]; generated: Readonly<Record<string, string>> };

/** Where a row's edit came from: this Environment, or delivered from another one. */
export type EditOrigin = "here" | "delivered";

/** A required decision on an edited row the release dropped; null until someone picks. */
export type Decision = "keep" | "remove" | null;

/** A step's state. A step waiting on a server that is gone waits until it is seen again or removed; nothing times out. */
export type StepState = "queued" | "running" | "done" | { waiting: string };

/**
 * One step of a topology change. A Deploy step is an ordinary Deployment of some members. A Job step is a long copy
 * on one server: `beside` steps hold no Deploy slot, so the next step starts with them.
 */
export type RunStep = {
  kind: "deploy" | "job";
  label: string;
  /** The server it runs on; null for a step across servers. */
  server: string | null;
  beside: boolean;
  state: StepState;
};

/** A topology change of an Install: its members before, and its steps once it runs. */
export type InstallRun = { from: readonly string[]; steps: RunStep[] | null };

/** All this prototype keeps for an Install here. Everything else lives on its Services. */
export type InstallRecord = {
  install: Install;
  receipt: Receipt;
  decisions: Readonly<Record<string, Decision>>;
  /** Which server each member runs on, frozen when a run starts. */
  placement: Readonly<Record<string, string>>;
  run: InstallRun | null;
};

/**
 * One option a release shows. A choice is the template's own value. A setting is one Setting of one of its Services
 * under the template's label: it is saved on the Service, so an update never replaces it.
 */
export type TemplateOption =
  | { kind: "choice"; key: string; label: string; choices: Readonly<Record<string, string>>; default: string | null;
    /** Shown only while another option has this value. */
    when?: readonly [key: string, value: string] }
  | { kind: "setting"; key: string; label: string; service: string; setting: ServiceSettingName | "domains" };

/** A nested template a release runs, and the member name it renders as. */
export type TemplateUse = { template: string; version: number; as: string };

export type TemplateRelease = { version: number; images: Readonly<Record<string, string>>; options: readonly TemplateOption[]; uses: readonly TemplateUse[] };

type Setting = string | number;

/** A Service a rule asks for: its image, the Settings it sets by path (`env.KEY`, `cpuLimit`), the nested template it came through. */
export type TemplateMember = { name: string; image: string; set: Readonly<Record<string, Setting>>; via: string | null };

/** What a rule asks for: its Services, and the values a parent may reference by key instead of its internals. */
export type Rendered = { members: TemplateMember[]; exports: Readonly<Record<string, string>>; distinctMachines?: boolean };

type Generated = Readonly<Record<string, string>>;
type RuleInput = { release: TemplateRelease; values: Values; name: string; generated: Generated; use: (as: string, values: Values) => Rendered };

/**
 * A template: its releases, oldest first, the one rule that turns option values into Services, and the values it
 * mints once per Install (passwords), which every later render reuses.
 */
export type Template = {
  id: string; name: string; releases: readonly [TemplateRelease, ...TemplateRelease[]];
  rule: (input: RuleInput) => Rendered;
  mint?: () => Generated;
};

const env = (pairs: Record<string, string | undefined>) =>
  Object.fromEntries(Object.entries(pairs).flatMap(([key, value]) => value === undefined ? [] : [[`env.${key}`, value]]));

const BACKUPS: TemplateOption = { kind: "choice", key: "backups", label: "Backups", choices: { daily: "Daily", off: "Off" }, default: "off" };

const POSTGRES_OPTIONS: TemplateOption[] = [
  { kind: "choice", key: "mode", label: "Mode", choices: { single: "Single", ha: "HA" }, default: "single" },
  { kind: "choice", key: "nodes", label: "Nodes", choices: { "3": "3", "5": "5" }, default: "3", when: ["mode", "ha"] },
  BACKUPS,
];

const postgresRelease = (version: number, major: string, spilo: string): TemplateRelease => ({
  version, options: POSTGRES_OPTIONS, uses: [],
  images: {
    db: `postgres:${major}-alpine`, member: `ghcr.io/zalando/spilo-${major}:${spilo}`, etcd: "quay.io/coreos/etcd:v3.5.17",
    router: "haproxy:3.0-alpine", backup: `prodrigestivill/postgres-backup-local:${major}`,
  },
});

/**
 * Single is one Service. HA is Patroni members that elect a leader through etcd, behind a router; the root stays
 * member 1, so its Volume never moves. Who leads is runtime state, never config. Parents reference `host`.
 */
const POSTGRES: Template = {
  id: "postgres",
  name: "Postgres",
  releases: [postgresRelease(1, "16", "3.3-p1"), postgresRelease(2, "17", "4.0-p2")],
  mint: () => ({ password: randomText("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789", 32) }),
  rule: ({ release: { version, images: { db = "", member = "", etcd = "", router = "", backup = "" } }, values: { mode, nodes, backups }, name, generated }) => {
    const ha = mode === "ha";
    // Installs from before passwords were minted read the old fixed one.
    const password = generated["password"] ?? "postgres";
    const names = Array.from({ length: ha ? Number(nodes ?? 3) : 1 }, (_, index) => index === 0 ? name : `${name}-${index + 1}`);
    const host = ha ? `${name}-router` : name;
    return {
      exports: { host },
      distinctMachines: ha,
      members: [
        ...names.map((node) => ({ name: node, image: ha ? member : db, via: null, set: env({
          POSTGRES_PASSWORD: password,
          POSTGRES_DB: ha ? undefined : "postgres",
          // v2 drops md5-era auth and turns on checksums.
          POSTGRES_HOST_AUTH_METHOD: !ha && version === 1 ? "scram-sha-256" : undefined,
          POSTGRES_INITDB_ARGS: !ha && version >= 2 ? "--data-checksums" : undefined,
          PATRONI_NAME: ha ? node : undefined,
          PATRONI_SCOPE: ha ? name : undefined,
          PATRONI_ETCD3_HOSTS: ha ? `${name}-etcd:2379` : undefined,
        }) })),
        ...ha ? [
          { name: `${name}-etcd`, image: etcd, via: null, set: env({ ETCD_NAME: `${name}-etcd` }) },
          { name: host, image: router, via: null, set: env({ PG_MEMBERS: names.join(",") }) },
        ] : [],
        ...backups === "daily"
          ? [{ name: `${name}-backup`, image: backup, via: null, set: env({ POSTGRES_HOST: host, POSTGRES_PASSWORD: password, SCHEDULE: "@daily" }) }]
          : [],
      ],
    };
  },
};

const SIZES = new Map(Object.entries({ s: { cpuLimit: 1, memLimit: 1 }, m: { cpuLimit: 2, memLimit: 4 }, l: { cpuLimit: 4, memLimit: 8 } }));
const REGISTRATION = new Map(Object.entries({ open: "false", invite: "invite_only", closed: "true" }));
const INCLUDED = { included: "Included", external: "External" };

const plausibleOptions = (registration: boolean): TemplateOption[] => [
  { kind: "setting", key: "domains", label: "Domains", service: "plausible", setting: "domains" },
  ...registration ? [{ kind: "choice", key: "registration", label: "Registration", choices: { open: "Open", invite: "Invite only", closed: "Closed" }, default: null } as const] : [],
  { kind: "choice", key: "size", label: "Size", choices: { s: "Small", m: "Medium", l: "Large" }, default: "s" },
  { kind: "setting", key: "replicas", label: "Web servers", service: "plausible", setting: "replicas" },
  { kind: "choice", key: "database", label: "Database", choices: INCLUDED, default: "included" },
  { ...BACKUPS, when: ["database", "included"] },
  { kind: "choice", key: "events", label: "Events", choices: INCLUDED, default: "included" },
];

const PLAUSIBLE: Template = {
  id: "plausible",
  name: "Plausible",
  releases: [
    { version: 2, options: plausibleOptions(false), uses: [{ template: "postgres", version: 1, as: "plausible-db" }],
      images: { web: "ghcr.io/plausible/community-edition:v2.1.4", events: "clickhouse/clickhouse-server:24.3-alpine" } },
    { version: 3, options: plausibleOptions(true), uses: [{ template: "postgres", version: 2, as: "plausible-db" }],
      images: { web: "ghcr.io/plausible/community-edition:v3.0.1", events: "clickhouse/clickhouse-server:24.12-alpine" } },
  ],
  rule: ({ release: { version, images: { web = "", events: clickhouse = "" } }, values: { size, registration, database, events, backups }, name, use }) => {
    const limits = SIZES.get(size ?? "s") ?? {};
    const db = database === "included" ? use(`${name}-db`, { backups: backups ?? null }) : null;
    // ponytail: "External" points at the Environment's `postgres` and leaves ClickHouse to the user.
    const host = db?.exports["host"] ?? "postgres";
    return { exports: {}, members: [
      { name, image: web, via: null, set: { ...limits, ...env({
        // v2 keeps registration closed; v3 asks.
        DISABLE_REGISTRATION: version === 2 ? "true" : REGISTRATION.get(registration ?? ""),
        DATABASE_URL: `postgres://postgres:postgres@${host}:5432/plausible`,
        CLICKHOUSE_DATABASE_URL: events === "included" ? `http://${name}-events:8123/plausible` : undefined,
      }) } },
      ...events === "included" ? [{ name: `${name}-events`, image: clickhouse, via: null, set: { ...limits, ...env({ CLICKHOUSE_SKIP_USER_SETUP: "1" }) } }] : [],
      ...db?.members ?? [],
    ] };
  },
};

// ponytail: a mock catalog until Cloud serves templates.
export const TEMPLATES: readonly Template[] = [PLAUSIBLE, POSTGRES];

export const templateById = (id: string) => TEMPLATES.find((template) => template.id === id) ?? null;

export const releaseOf = (template: Template, version: number) =>
  template.releases.find((release) => release.version === version) ?? template.releases[0];

/** Each choice's value, its default where nothing was picked. */
export const withDefaults = (release: TemplateRelease, values: Values): Values => Object.fromEntries(release.options
  .flatMap((option) => option.kind === "choice" ? [[option.key, values[option.key] ?? option.default]] : []));


/** What `release` of `template` asks for with `values`, nested templates' Services included. */
export function render(template: Template, version: number, values: Values, name = template.id, generated: Generated = {}): Rendered {
  const release = releaseOf(template, version);
  return template.rule({
    release, name, generated, values: withDefaults(release, values),
    use: (as, nestedValues) => {
      const use = release.uses.find((candidate) => candidate.as === as);
      const nested = use && templateById(use.template);
      if (!use || !nested) return { members: [], exports: {} };
      const child = render(nested, use.version, nestedValues, as, generated);
      return { exports: child.exports, members: child.members.map((member) => ({ ...member, via: member.via ?? nested.id })) };
    },
  });
}

export const members = (template: Template, version: number, values: Values, name = template.id) =>
  render(template, version, values, name).members;

type ChoiceOption = Extract<TemplateOption, { kind: "choice" }>;

/** An input as it stands: what was set, else the release's default, else not set. */
export function inputOf(option: ChoiceOption, inputs: Install["inputs"]): Input {
  const input = inputs[option.key];
  if (input && input.state !== "unset") return input;
  return option.default === null ? { state: "unset" } : { state: "value", value: option.default };
}

/** The values a rule reads. A conflict renders what is here until someone picks. */
export const valuesOf = (inputs: Install["inputs"]): Values => Object.fromEntries(Object.entries(inputs)
  .map(([key, input]) => [key, input.state === "value" ? input.value : input.state === "conflict" ? input.here : null]));

/** The choices a release shows with these inputs. */
export const shownOptions = (release: TemplateRelease, inputs: Install["inputs"]) => release.options.filter((option) =>
  option.kind !== "choice" || !option.when || valuesOf(inputs)[option.when[0]] === option.when[1]);

/** A new Install of `template` at its newest release: inputs at their defaults, values minted once. */
export function newInstall(template: Template, name: string): InstallRecord {
  const release = template.releases.at(-1) ?? template.releases[0];
  const generated = template.mint?.() ?? {};
  const rendered = render(template, release.version, {}, name, generated);
  return {
    install: { name, template: template.id, version: release.version, inputs: {}, distinctMachines: rendered.distinctMachines ?? false },
    receipt: { version: release.version, members: rendered.members, generated },
    decisions: {}, placement: {}, run: null,
  };
}

/** Each Setting a member's rendered row sets, by field. */
const rowsOf = (member: TemplateMember) => new Map(Object.entries({ image: member.image, ...member.set }).map(([field, value]) => [field, String(value)]));

/** A row of a member changed away from what the Install set. */
export type EditedRow = { service: string; field: string; path: string; value: string; base: string; origin: EditOrigin };

/** An Install running here, as screens read it. */
export type InstallView = {
  template: Template;
  record: InstallRecord;
  install: Install;
  release: TemplateRelease;
  /** The newest release, when it is newer than `release`. */
  latest: TemplateRelease | null;
  /** Its Services here. */
  members: ServiceListing[];
  /** Its generated base's members. */
  rendered: TemplateMember[];
  edited: EditedRow[];
  /** Inputs not set or in conflict, and drops not decided: each blocks Deploy. */
  toFix: number;
};

function editedRows(record: InstallRecord, services: readonly ServiceListing[], settings: EnvironmentView, parent: EnvironmentView | null): EditedRow[] {
  const generated = new Set(Object.values(record.receipt.generated));
  return record.receipt.members.filter((member) => services.some((service) => service.name === member.name)).flatMap((member) => {
    const rows = serviceSettingRows(settings, member.name);
    const parentRows = parent ? serviceSettingRows(parent, member.name) : null;
    return [...rowsOf(member)].flatMap(([field, base]) => {
      const value = settingText(rows.get(field)?.value);
      if (value === base || generated.has(base)) return [];
      // ponytail: origin is inferred from the Parent's value until the Config Store records it per row.
      const origin: EditOrigin = parentRows && settingText(parentRows.get(field)?.value) === value ? "delivered" : "here";
      return [{ service: member.name, field, path: `${member.name}.${field}`, value, base, origin }];
    });
  });
}

function view(template: Template, record: InstallRecord, own: ServiceListing[], services: readonly ServiceListing[],
  settings: EnvironmentView, parent: EnvironmentView | null): InstallView {
  const release = releaseOf(template, record.install.version);
  const newest = template.releases.at(-1) ?? release;
  const inputs = release.options.flatMap((option) => option.kind === "choice" && shownOptions(release, record.install.inputs).includes(option)
    ? [inputOf(option, record.install.inputs)] : []);
  return {
    template, record, release, install: record.install, members: own, rendered: record.receipt.members,
    latest: newest.version > release.version ? newest : null,
    edited: editedRows(record, services, settings, parent),
    toFix: inputs.filter((input) => input.state !== "value").length + Object.values(record.decisions).filter((decision) => decision === null).length,
  };
}

/** The record of Services stamped before this Environment kept one: their stamp's release, at its defaults. */
function stampedRecord(template: Template, version: number): InstallRecord {
  const rendered = render(template, version, {}, template.id);
  return {
    install: { name: template.id, template: template.id, version, inputs: {}, distinctMachines: rendered.distinctMachines ?? false },
    receipt: { version, members: rendered.members, generated: {} },
    decisions: {}, placement: {}, run: null,
  };
}

/**
 * The Installs here. Each kept record claims the stamped Services its base names; stamped Services left over form one
 * Install per template, named after it. `parent` is the Environment a Branch was made from, for where edits came from.
 */
export function installs(services: readonly ServiceListing[], settings: EnvironmentView,
  records: Readonly<Record<string, InstallRecord>> | null, parent: EnvironmentView | null = null): InstallView[] {
  const claimed = new Set<string>();
  const claim = (template: Template, names: ReadonlySet<string>) => services.filter((service) => {
    if (service.template?.id !== template.id || !names.has(service.name) || claimed.has(service.id)) return false;
    claimed.add(service.id);
    return true;
  });
  const kept = Object.values(records ?? {}).flatMap((record) => {
    const template = templateById(record.install.template);
    if (!template) return [];
    const own = claim(template, new Set(record.receipt.members.map((member) => member.name)));
    return own.length > 0 ? [view(template, record, own, services, settings, parent)] : [];
  });
  const stamped = TEMPLATES.flatMap((template) => {
    const root = services.find((service) => service.template?.id === template.id && service.name === template.id && !claimed.has(service.id));
    if (!root?.template || records?.[template.id]) return [];
    const record = stampedRecord(template, root.template.version);
    const own = claim(template, new Set(record.receipt.members.map((member) => member.name)));
    return own.length > 0 ? [view(template, record, own, services, settings, parent)] : [];
  });
  return [...kept, ...stamped];
}

/** The Install a Service belongs to here; null when it is its own. */
export const installOf = (views: readonly InstallView[], serviceId: string) =>
  views.find((candidate) => candidate.members.some((member) => member.id === serviceId)) ?? null;

/** One row of a merge: what is here, what the base had, and what the target sets (null where it dropped the row). */
export type MergeRow = { service: string; field: string; path: string; here: string; base: string | null; target: string | null };

/**
 * What moving an Install to a release and inputs does: a three-way merge of its base, the target's render, and what
 * is here. Rows nobody edited take the target. Edited rows stay. An edited row the target drops needs a decision.
 */
export type InstallPlan = {
  record: InstallRecord;
  added: TemplateMember[];
  /** Their Volumes stay. */
  removed: ServiceListing[];
  changes: MergeRow[];
  kept: MergeRow[];
  decide: MergeRow[];
};

export function planInstall(current: InstallView, version: number, inputs: Install["inputs"],
  services: readonly ServiceListing[], settings: EnvironmentView): InstallPlan {
  const { template, record } = current;
  const rendered = render(template, version, valuesOf(inputs), record.install.name, record.receipt.generated);
  const base = new Map(record.receipt.members.map((member) => [member.name, rowsOf(member)]));
  const changes: MergeRow[] = [];
  const kept: MergeRow[] = [];
  const decide: MergeRow[] = [];
  const names = new Set([...base.keys(), ...rendered.members.map((member) => member.name)]);
  for (const name of names) {
    if (!services.some((service) => service.name === name)) continue;
    const rows = serviceSettingRows(settings, name);
    const before = base.get(name) ?? new Map<string, string>();
    const member = rendered.members.find((candidate) => candidate.name === name);
    const after = member ? rowsOf(member) : new Map<string, string>();
    for (const field of new Set([...before.keys(), ...after.keys()])) {
      const row: MergeRow = { service: name, field, path: `${name}.${field}`, here: settingText(rows.get(field)?.value),
        base: before.get(field) ?? null, target: after.get(field) ?? null };
      const edited = row.base !== null && row.here !== row.base;
      if (!edited) {
        if (row.target !== row.base) changes.push(row);
      } else if (row.target === null && member) decide.push(row);
      else if (member) kept.push(row);
    }
  }
  return {
    added: rendered.members.filter((member) => !services.some((service) => service.name === member.name)),
    removed: current.members.filter((service) => !rendered.members.some((member) => member.name === service.name)),
    changes, kept, decide,
    record: {
      ...record,
      install: { ...record.install, version, inputs, distinctMachines: rendered.distinctMachines ?? false },
      receipt: { version, members: rendered.members, generated: record.receipt.generated },
      decisions: { ...record.decisions, ...Object.fromEntries(decide.map((row) => [row.path, null])) },
    },
  };
}

/** A server the mock runtime driver shows. */
export type MockServer = { name: string; online: boolean };

/** Members placed on servers before anything changes; refused up front when the members need more servers than are up. */
export type Placement =
  | { ok: true; placement: Record<string, string> }
  | { ok: false; needs: number; available: number };

/** A new mock server, named by the first number no server here has, so a removed server's name is never reused twice. */
export function nextServer(servers: readonly MockServer[]): MockServer {
  const names = new Set(servers.map((server) => server.name));
  let number = 1;
  while (names.has(`m${number}`)) number += 1;
  return { name: `m${number}`, online: true };
}

export function place(names: readonly string[], servers: readonly MockServer[], keep: Readonly<Record<string, string>>, distinct: boolean): Placement {
  const online = servers.filter((server) => server.online).map((server) => server.name);
  const needs = distinct ? names.length : 1;
  if (online.length < needs) return { ok: false, needs, available: online.length };
  const placement: Record<string, string> = {};
  const used = new Set<string>();
  for (const name of names) {
    const kept = keep[name];
    if (kept && online.includes(kept) && !(distinct && used.has(kept))) { placement[name] = kept; used.add(kept); }
  }
  for (const name of names) {
    if (placement[name]) continue;
    const free = online.find((server) => !distinct || !used.has(server)) ?? online[0] ?? "";
    placement[name] = free;
    used.add(free);
  }
  return { ok: true, placement };
}

const step = (kind: RunStep["kind"], label: string, server: string | null, beside = false): RunStep =>
  ({ kind, label, server, beside, state: "queued" });

/**
 * A topology change as steps. Into HA: etcd, the root as leader, a copy per replica on its server beside the router's
 * Deploy, then the replicas. Out of HA: repoint the export, redeploy consumers, drop router and etcd, then each
 * replica on its own server.
 */
export function runSteps(from: readonly string[], to: readonly string[], root: string, placement: Readonly<Record<string, string>>): RunStep[] {
  const added = to.filter((name) => !from.includes(name));
  const removed = from.filter((name) => !to.includes(name));
  const replica = (name: string) => name !== root && !name.endsWith("-etcd") && !name.endsWith("-router");
  const infra = (names: readonly string[], suffix: string) => names.filter((name) => name.endsWith(suffix));
  if (added.length > 0) {
    const replicas = added.filter(replica);
    return [
      ...infra(added, "-etcd").map((name) => step("deploy", `Deploy ${name}`, placement[name] ?? null)),
      step("deploy", `Deploy ${root}`, placement[root] ?? null),
      ...replicas.map((name) => step("job", `Basebackup ${name}`, placement[name] ?? null, true)),
      ...infra(added, "-router").map((name) => step("deploy", `Deploy ${name}`, placement[name] ?? null)),
      ...replicas.length > 0 ? [step("deploy", `Deploy ${replicas.join(", ")}`, null)] : [],
    ];
  }
  const leaving = removed.filter((name) => !replica(name));
  return [
    step("deploy", `Repoint ${root}`, placement[root] ?? null),
    step("deploy", "Redeploy consumers", null),
    ...leaving.length > 0 ? [step("deploy", `Remove ${leaving.join(", ")}`, null)] : [],
    ...removed.filter(replica).map((name) => step("deploy", `Remove ${name}`, placement[name] ?? null, true)),
  ];
}

/**
 * The mock runtime's next tick. Running steps finish, or wait on their server while it is down. Then the next queued
 * step starts, and while it runs beside Deploys the one after it starts too.
 */
export function advance(steps: readonly RunStep[], servers: readonly MockServer[]): RunStep[] {
  const up = (server: string | null) => server === null || servers.some((candidate) => candidate.name === server && candidate.online);
  const next = steps.map((current): RunStep => {
    if (current.state === "running" || typeof current.state === "object") {
      return up(current.server) ? { ...current, state: "done" } : { ...current, state: { waiting: current.server ?? "" } };
    }
    return current;
  });
  // A step that is not beside the others holds the ones after it until it is done.
  if (next.some((candidate) => !candidate.beside && typeof candidate.state === "object")) return next;
  for (let index = next.findIndex((candidate) => candidate.state === "queued"); index >= 0 && index < next.length; index += 1) {
    const current = next[index];
    if (!current || current.state !== "queued") break;
    next[index] = { ...current, state: "running" };
    if (!current.beside) break;
  }
  return next;
}

/** The way out of a step waiting on a server: the server goes, and its steps finish without it. */
export const removeServer = (steps: readonly RunStep[], server: string): RunStep[] => steps.map((current) =>
  typeof current.state === "object" && current.state.waiting === server ? { ...current, state: "done" } : current);
