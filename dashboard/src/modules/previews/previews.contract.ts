/**
 * Previews as the Store answers them, with the JSON field names DESIGN.md (NIC-33) fixes between the Store, the rule
 * draft and this dashboard. The generated SDK doesn't carry them yet: once it does, import each type from `@ployz/sdk`,
 * `previewRuleBody` and `previewRuleEndBody` from `@ployz/sdk/authority`, and drop this file's guards.
 */
import type {
  AutoDeployRule, CommitSha, ConfigCommand, ConfigQuery, ConfigView, DeploymentId, EnvironmentName, GithubTimestamp, MachineId, Namespace,
  ProjectName, ProjectSummary, PullRequestNumber, RepositoryId, RepositoryName, RequestedServiceSpec, RuleDraft, RuleImageView,
  RuleSourceView, RuleTerms, Signed, SignerKey, Timestamp,
} from "@ployz/sdk";
import * as authority from "@ployz/sdk/authority";
import { Schema } from "effect";
import { StoreCommand, StoreQuery } from "#/modules/config-store/store.contract";

/** One repository's previews in a Project, and the pull requests that may have one. */
export type PreviewsQuery = { project: ProjectName | null; repository: RepositoryName; number: PullRequestNumber | null };

export type PreviewsView = {
  project: ProjectSummary;
  repository: RepositoryName;
  repository_id: RepositoryId;
  /** None while previews are off or were never set up. */
  settings: PreviewSettings | null;
  /** Deploying and Live previews, across every Server of the settings. */
  live: number;
  limit: number;
  /** Open pull requests, and the asked number even when closed, newest push first. */
  pull_requests: PreviewRow[];
};

export type PreviewSettings = {
  start_from: EnvironmentName;
  servers: MachineId[];
  limit: number;
  generation: number;
  signer: SignerKey;
  issued_at: Timestamp;
  rule: Signed;
};

export type PreviewRow = {
  number: PullRequestNumber;
  title: string;
  author: string;
  author_id?: number;
  member: boolean;
  fork: boolean;
  head: CommitSha | null;
  updated: GithubTimestamp;
  open: boolean;
  preview: PreviewOutcome;
};

export type PreviewOutcome =
  | { state: "not_deployed"; why?: NotDeployed }
  | { state: "deploying"; environment: EnvironmentName; deployment: DeploymentId; head: CommitSha }
  | { state: "live"; environment: EnvironmentName; address?: string; head: CommitSha; server?: MachineId; newer?: CommitSha }
  | { state: "closed" };

export type NotDeployed =
  | { kind: "full"; live: number; limit: number }
  | { kind: "not_covered"; service: string }
  | { kind: "off" }
  | { kind: "merges_into_start_from" }
  | { kind: "no_server" }
  | { kind: "not_member"; author: string }
  | { kind: "failed"; message: string };

/** What a preview rule for `repository` copying `start_from` would cover. */
export type PreviewRuleQuery = { project: ProjectName | null; repository: RepositoryName; start_from: EnvironmentName };

/** A Server previews may run on: one that can't hold them is shown greyed with why. */
export type PreviewServer = { machine_id: MachineId; holds: boolean; why?: string };

export type PreviewRuleView = {
  /** The start-from Environment's ID and name. */
  parent: string;
  parent_name: EnvironmentName;
  namespace: Namespace;
  sample: Namespace;
  /** The kept preview rule, ended or not. */
  rule?: Signed;
  sources: RuleSourceView[];
  images: RuleImageView[];
  target: RequestedServiceSpec[];
  volumes?: unknown;
  /** Names of the start-from's Sealed variables a preview opens. */
  secrets: string[];
  servers: PreviewServer[];
  limit: number;
};

export type DeployPreview = { command: "deploy_preview"; repository_id: RepositoryId; number: PullRequestNumber; head: CommitSha };
export type ClosePreview = { command: "close_preview"; project: ProjectName; environment: EnvironmentName };

export const previewsQuery = (query: PreviewsQuery) => ({ query: "previews", ...query }) as const;
export const previewRuleQuery = (query: PreviewRuleQuery) => ({ query: "preview_rule", ...query }) as const;

/** The query kinds above, as the change stream refreshes Store views by kind. */
export type PreviewQueryKind = "previews" | "preview_rule";

const isStoreQuery = Schema.is(StoreQuery);
const isStoreCommand = Schema.is(StoreCommand);

/** `query` through the Store's one door, which checks only that it names its kind and decodes the rest itself. */
export function asStoreQuery(query: ReturnType<typeof previewsQuery> | ReturnType<typeof previewRuleQuery>): ConfigQuery {
  const named: unknown = query;
  if (!isStoreQuery(named)) throw new Error("A preview query names no kind.");
  return named;
}

/** `command` through the Store's one door, as `asStoreQuery`. */
export function asStoreCommand(command: DeployPreview | ClosePreview): ConfigCommand {
  const named: unknown = command;
  if (!isStoreCommand(named)) throw new Error("A preview command names no kind.");
  return named;
}

const viewNamed = <T>(kind: PreviewQueryKind) => Schema.is(Schema.declare<T>((value): value is T =>
  typeof value === "object" && value !== null && "view" in value && value.view === kind));
const isPreviewsView = viewNamed<PreviewsView>("previews");
const isPreviewRuleView = viewNamed<PreviewRuleView>("preview_rule");

/** `view` as a previews view, if the Store answered with one: each query kind's view has its name. */
export function previewsViewOf(view: ConfigView) {
  const answered: unknown = view;
  return isPreviewsView(answered) ? answered : undefined;
}

/** `view` as a preview rule view, if the Store answered with one. */
export function previewRuleViewOf(view: ConfigView) {
  const answered: unknown = view;
  return isPreviewRuleView(answered) ? answered : undefined;
}

/** Whether a Store read drafts a preview rule, so the Store needs the Servers that can hold a preview. */
export function draftsPreviewRule(query: ConfigQuery) {
  const kind: string = query.query;
  return kind === "preview_rule";
}

const PREVIEW_COMMANDS = ["deploy_preview", "close_preview"] as const;

/** `command`'s kind among the preview commands, if it is one. Each admits a Deployment Cloud's worker runs. */
export function previewCommand(command: ConfigCommand) {
  const kind: string = command.command;
  return PREVIEW_COMMANDS.find((candidate) => candidate === kind) ?? null;
}

/** A kept rule's body as ployz-core wrote it. */
export function ruleBody(rule: Signed): AutoDeployRule | null {
  try {
    // SAFETY: the Store keeps only a rule body it parsed as an `AutoDeployRule`.
    const body: AutoDeployRule = JSON.parse(rule.body);
    return body;
  } catch {
    return null;
  }
}

type Drafts = (view: PreviewRuleView, terms: RuleTerms) => RuleDraft;
type Ends = (view: PreviewRuleView, terms: Pick<RuleTerms, "issued_at" | "signer">) => RuleDraft;
const isFunction = <T>() => Schema.is(Schema.declare<T>((value): value is T => typeof value === "function"));

/** `@ployz/sdk/authority`, which gains the preview rule's drafts with ployz-core's `draft_preview`. */
const exports: {
  readonly autoDeployRuleEndBody: unknown;
  readonly previewRuleBody?: unknown;
  readonly previewRuleEndBody?: unknown;
} = authority;

function authorityExport<T>(name: "previewRuleBody" | "previewRuleEndBody"): T {
  const exported = exports[name];
  if (!isFunction<T>()(exported)) throw new Error("This browser can't draft a preview rule yet. Reload once Ployz Cloud updates.");
  return exported;
}

/** The preview rule ployz-core drafts for `view` under `terms`, as the Servers check it. */
export const previewRuleBody: Drafts = (view, terms) => authorityExport<Drafts>("previewRuleBody")(view, terms);

/** The ending generation of the preview rule `view` keeps, which turns previews off. */
export const previewRuleEndBody: Ends = (view, terms) => authorityExport<Ends>("previewRuleEndBody")(view, terms);
