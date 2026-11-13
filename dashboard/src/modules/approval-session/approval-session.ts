/**
 * A browser approval session. One passkey prompt lends a short-lived browser key the right to approve deploys,
 * restarts, stops and starts in one Environment for an hour. Each press then carries approvals signed by that key,
 * beside the passkey's grant of it. Servers check the chain against the passkeys they trust; Cloud only carries it and
 * never holds the key.
 *
 * The shapes and bytes follow `ployz_core::authority`: canonical JSON, the `ployz-authority/v1` signing input, and the
 * grant challenge `SHA-256(signing input of the grant body)`.
 */
import type { ActionFamily, DeployTerms, PlanView, ServiceVerb, SessionGrant, Signed } from "@ployz/sdk";
import { deployApprovalBody } from "@ployz/sdk/authority";
import { Schema } from "effect";
import type { JsonValue } from "#/db/tables";
import { asBoolean, asFiniteNumber, asRecord, asString } from "#/lib/json";
import { shownText } from "#/modules/github/preview-review";
import { type OlderServer, olderServer } from "#/modules/runtime/daemon-compat";
import { storeVersion } from "#/modules/volume-placement/volume-placement";
import { base64url, passkeyAnswer, PasskeyRefused, signingInput, statementChallenge } from "./passkey-statement";

export { base64url, signingInput };

/** How long one passkey prompt lasts. Servers cap a grant at an hour. */
export const SESSION_MINUTES = 60;
/**
 * How long one approval lasts. Servers cap it at `validity::MAX_LIFETIME_MILLIS`. A session with less than this left
 * prompts again, so every approval gets its whole window inside the grant and a rolling restart can't outlive it.
 */
const APPROVAL_MS = 15 * 60_000;

/**
 * Sorted, as Servers parse them. Each `ServiceVerb` is the family of the same name. No `settings`: a settings change
 * reaches a Server only through a Deploy, which the deploy approval covers.
 */
export const SESSION_FAMILIES = ["deploy", "restart", "start", "stop"] as const satisfies readonly ActionFamily[];

/** Where a press acts: the Environment, and the Cluster and Servers its grant must name. */
export type ApprovalScope = {
  /** The Environment's ID, which a Sealed value names: a Server opens only Sealed values sealed for it under this grant. */
  readonly environmentId: string;
  readonly environmentName: string;
  readonly namespace: string;
  /** The Cluster's Sealing public key, as Servers report it. */
  readonly cluster: string;
  readonly machines: readonly string[];
};

/** The public half of a live session. Its browser key's private half lives only in this module. */
export type ApprovalSession = {
  readonly scope: ApprovalScope;
  readonly families: readonly ActionFamily[];
  readonly issuedAt: number;
  readonly expiresAt: number;
  /** The browser key's code, by the Device code rule: the first 10 base32 characters of its SHA-256. */
  readonly keyCode: string;
};

/** `proposal`: a proposal always prompts, so the one press that signs the whole plan is the person's own. */
export type PromptReason = "first" | "expired" | "scope" | "proposal";

let current: { readonly session: ApprovalSession; readonly grant: SessionGrant; readonly publicKey: string; readonly key: CryptoKeyPair } | null = null;
const listeners = new Set<() => void>();

export function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => void listeners.delete(listener);
}

export const readSession = (): ApprovalSession | null => current?.session ?? null;

const covers = (session: ApprovalSession, scope: ApprovalScope) =>
  session.scope.environmentId === scope.environmentId
  && session.scope.namespace === scope.namespace
  && session.scope.cluster === scope.cluster
  && scope.machines.every((machine) => session.scope.machines.includes(machine));

/** Why a press in `scope` must prompt, or null while the session covers it. */
export function promptReason(session: ApprovalSession | null, scope: ApprovalScope, family: ActionFamily, now = Date.now()): PromptReason | null {
  if (session === null) return "first";
  if (session.expiresAt - now < APPROVAL_MS) return "expired";
  if (!covers(session, scope) || !session.families.includes(family)) return "scope";
  return null;
}

/** `ployz_core::authority::canonical`: sorted ASCII keys, no whitespace, no null, only safe non-negative integers. */
export function canonicalJson(value: JsonValue): string {
  if (value === null) throw new Error("A signed body holds no null.");
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`;
  const text = asString(value);
  if (text !== null) return JSON.stringify(text);
  const number = asFiniteNumber(value);
  if (number !== null) {
    if (!Number.isSafeInteger(number) || number < 0) throw new Error("A signed body holds only safe non-negative integers.");
    return String(number);
  }
  const flag = asBoolean(value);
  if (flag !== null) return String(flag);
  const entries = Object.entries(asRecord(value) ?? {});
  if (entries.some(([key]) => !/^[\x20-\x7e]*$/.test(key))) throw new Error("A signed body's keys are ASCII.");
  entries.sort(([left], [right]) => (left < right ? -1 : left > right ? 1 : 0));
  return `{${entries.map(([key, item]) => `${JSON.stringify(key)}:${canonicalJson(item)}`).join(",")}}`;
}

const encoder = new TextEncoder();

export const hex = (bytes: ArrayBuffer | Uint8Array) =>
  Array.from(bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes), (byte) => byte.toString(16).padStart(2, "0")).join("");

const BASE32 = "abcdefghijklmnopqrstuvwxyz234567";

/** The Device code rule: the first 10 base32 characters of the SHA-256 of a key's bytes. */
export async function keyCode(key: Uint8Array<ArrayBuffer>): Promise<string> {
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", key));
  let bits = 0, value = 0, code = "";
  for (const byte of digest) {
    value = ((value << 8) | byte) & 0xffff;
    bits += 8;
    while (bits >= 5 && code.length < 10) {
      code += BASE32[(value >>> (bits - 5)) & 31];
      bits -= 5;
    }
    if (code.length === 10) break;
  }
  return code;
}

/** The grant body a passkey approves for `scope`, exactly as Servers parse it. */
export function grantBody(scope: ApprovalScope, browserKey: string, issuedAt: number): string {
  return canonicalJson({
    browser_key: browserKey,
    cluster: scope.cluster,
    delegation: "none",
    environment: scope.namespace,
    environment_id: scope.environmentId,
    expires_at: issuedAt + SESSION_MINUTES * 60_000,
    families: [...SESSION_FAMILIES],
    issued_at: issuedAt,
    machines: [...new Set(scope.machines)].sort(),
  });
}

export const grantChallenge = (body: string) => statementChallenge(body, "workflow");

/** Why a session could not start, in words the prompt shows as they are. */
export class SessionRefused extends Error {}

async function browserKey(): Promise<CryptoKeyPair> {
  try {
    // Non-extractable: the private half can sign in this page and can never be read out of it.
    return await crypto.subtle.generateKey({ name: "Ed25519" }, false, ["sign", "verify"]);
  } catch {
    throw new SessionRefused("This browser can't use a passkey here. Use the CLI instead.");
  }
}

/**
 * One passkey prompt. The grant body names the browser key, the Environment and its Cluster and Servers, the actions and
 * the hour; the passkey answers its digest, so the grant covers that body and nothing else.
 */
export async function startSession(scope: ApprovalScope, now = Date.now()): Promise<ApprovalSession> {
  const key = await browserKey();
  const raw = new Uint8Array(await crypto.subtle.exportKey("raw", key.publicKey));
  const publicKey = hex(raw);
  const body = grantBody(scope, publicKey, now);
  let assertion;
  try {
    assertion = await passkeyAnswer(await grantChallenge(body));
  } catch (error) {
    throw error instanceof PasskeyRefused ? new SessionRefused(error.message) : error;
  }
  const grant: SessionGrant = { body, assertion };
  const session: ApprovalSession = {
    scope,
    families: SESSION_FAMILIES,
    issuedAt: now,
    expiresAt: now + SESSION_MINUTES * 60_000,
    keyCode: await keyCode(raw),
  };
  current = { session, grant, publicKey, key };
  listeners.forEach((listener) => listener());
  return session;
}

/** `hook`: a pre-deploy hook's container, which a run only ever stops. */
export type ActCopy = { readonly machine: string; readonly container: string; readonly hook: boolean };

const operationId = () => hex(crypto.getRandomValues(new Uint8Array(16)));

/** A session's live key and grant, once it covers `scope`, every family and every copy's Server; else the prompt asks again. */
function signer(scope: ApprovalScope, families: readonly ActionFamily[], copies: readonly ActCopy[], now: number) {
  const refused = current === null || families.some((family) => promptReason(current?.session ?? null, scope, family, now) !== null)
    || copies.some(({ machine }) => !scope.machines.includes(machine));
  if (refused || current === null) throw new SessionRefused("Confirm it's you again.");
  return current;
}

type Window = { readonly operation: string; readonly issuedAt: number; readonly expiresAt: number };

/** A copy's approval body for `verb`, exactly as Servers parse it: a hook's container only ever gets a stop. */
function approvalBody(session: ApprovalSession, publicKey: string, verb: ServiceVerb, scope: ApprovalScope, service: string, copy: ActCopy, window: Window, proposal?: { operation: string; index: number }) {
  const body = {
    cluster: session.scope.cluster,
    container: copy.container,
    expires_at: window.expiresAt,
    issued_at: window.issuedAt,
    kind: copy.hook ? "stop" : verb,
    machines: [copy.machine],
    namespace: scope.namespace,
    operation: window.operation,
    service,
    signer: { browser: publicKey },
  };
  return canonicalJson(proposal === undefined ? body : { ...body, proposal: { index: proposal.index, operation: proposal.operation } });
}

/** A start skips a hook's container: a hook's approval can never start it. */
const signable = (verb: ServiceVerb, copies: readonly ActCopy[]) => copies.filter((copy) => !copy.hook || verb !== "start");

/**
 * One approval per container of `service`, each signed by the session's browser key and lasting its full 15 minutes,
 * which the session still covers. Keyed by container ID, as Cloud forwards them. Signs only when the session covers
 * `scope` and `verb` and each copy's Server, so a session for one scope never signs another. A hook's container gets
 * a stop approval at most, which can never start it: a start skips it, and a restart or stop only stops it.
 */
export async function approveAct(verb: ServiceVerb, scope: ApprovalScope, service: string, copies: readonly ActCopy[], now = Date.now()): Promise<Record<string, Signed>> {
  const { session, grant, publicKey, key } = signer(scope, [verb], copies, now);
  const entries = await Promise.all(signable(verb, copies).map(async (copy) => {
    const body = approvalBody(session, publicKey, verb, scope, service, copy, { operation: operationId(), issuedAt: now, expiresAt: now + APPROVAL_MS });
    const signature = hex(await crypto.subtle.sign({ name: "Ed25519" }, key.privateKey, signingInput(body)));
    return [copy.container, { body, signatures: [{ key: publicKey, signature }], grants: [grant] } satisfies Signed] as const;
  }));
  return Object.fromEntries(entries);
}

/** One step of a proposal as the browser signs it: the verb, the Service as Servers name it, and its copies. */
export type ProposalStepTarget = { readonly verb: ServiceVerb; readonly runtime: string; readonly copies: readonly ActCopy[] };

/** A step's approval for one container: unsigned itself, it rides inside the bundle that lists its digest. */
export type StepEnvelope = Signed & { readonly proposal: Signed };

/** Per step index, one approval per container, each riding inside the one bundle the browser key signed. */
export type ProposalApprovals = Record<string, Record<string, StepEnvelope>>;

export const stepDigest = async (body: string) => hex(await crypto.subtle.digest("SHA-256", encoder.encode(body)));

/**
 * One signature over a whole plan. Each step's copy gets an unsigned approval body naming the bundle and its place in
 * it; the bundle lists every body's digest in order, under the plan's one window, and the browser key signs only the
 * bundle. A Server runs a step only when its body hashes to the digest at its place, so nothing can be added or swapped
 * afterwards; the shared `operation` and the step's place let a Server's consumption log stop a replay.
 */
export async function approveProposal(steps: readonly ProposalStepTarget[], scope: ApprovalScope, now = Date.now()): Promise<{ bundle: Signed; approvals: ProposalApprovals }> {
  const { session, grant, publicKey, key } = signer(scope, steps.map((step) => step.verb), steps.flatMap((step) => step.copies), now);
  const window = { operation: operationId(), issuedAt: now, expiresAt: now + APPROVAL_MS };
  const bodies: { at: number; container: string; body: string; kind: ServiceVerb }[] = [];
  for (const [at, step] of steps.entries()) {
    for (const copy of signable(step.verb, step.copies)) {
      const body = approvalBody(session, publicKey, step.verb, scope, step.runtime, copy, window, { operation: window.operation, index: bodies.length });
      bodies.push({ at, container: copy.container, body, kind: copy.hook ? "stop" : step.verb });
    }
  }
  if (bodies.length === 0) throw new SessionRefused("This proposal has nothing to run.");
  const listed = await Promise.all(bodies.map(async ({ body, kind }, index) => ({ digest: await stepDigest(body), index, kind })));
  const body = canonicalJson({
    cluster: session.scope.cluster,
    environment: scope.namespace,
    expires_at: window.expiresAt,
    issued_at: window.issuedAt,
    operation: window.operation,
    signer: { browser: publicKey },
    steps: listed,
  });
  const signature = hex(await crypto.subtle.sign({ name: "Ed25519" }, key.privateKey, signingInput(body)));
  const bundle: Signed = { body, signatures: [{ key: publicKey, signature }], grants: [grant] };
  const approvals: ProposalApprovals = Object.fromEntries(steps.map((_, at) => [String(at), {}]));
  for (const { at, container, body } of bodies) {
    approvals[String(at)] = { ...approvals[String(at)], [container]: { body, signatures: [], proposal: bundle } };
  }
  return { bundle, approvals };
}

/** Each Server must take a browser's deploy approval, under a grant naming its Environment, before one goes to it. */
export const SESSION_GRANT_CAPABILITY = "ployz.restart.session-grant.v2";
const DEPLOY_CAPABILITIES = ["ployz.container.deploy-approval.v1", SESSION_GRANT_CAPABILITY] as const;
/** Each Server must take an automatic deploy rule a passkey answered with its own prompt before one goes to it. */
export const RULE_CAPABILITIES = ["ployz.container.deploy-rule.v1", "ployz.rule.passkey.v1"] as const;
/** Each Server must take a browser's approval of a redeploy's commits, in place of its branch head, before one goes to it. */
export const COMMIT_CAPABILITIES = ["ployz.rule.commit-approval.v1", SESSION_GRANT_CAPABILITY] as const;

/** A Store plan read with `target`, as the browser signs a Deploy of it. */
export type DeployPlan = Pick<PlanView, "version" | "namespace" | "target">;

/**
 * What a Deploy's approval holds, and what it shows the person: an `Approved web: nginx@sha256:…` line per Service, then
 * each setting it signs, indented, then the terms it signs them under.
 */
export type DeployApproved = { readonly approval: Signed; readonly lines: readonly string[] };

/**
 * Approvals this browser drafted under the session, before it signs them: the exact bodies it will sign, every term they
 * hold as the person reads it, and the session key and window they name.
 */
export type Drafted = { readonly bodies: readonly string[]; readonly lines: readonly string[]; readonly signer: string; readonly expiresAt: number };

const fromHex = (text: string) => new Uint8Array((text.match(/../g) ?? []).map((pair) => Number.parseInt(pair, 16)));

/** `2026-11-11T03:19:09Z`: a time a body signs, in UTC, as the CLI prints it. */
export const utc = (at: number) => new Date(at).toISOString().replace(/\.\d{3}Z$/, "Z");

/** The terms every body of one draft shares, as this browser wrote them: the Cluster, the signer and the window. */
const SharedTerms = Schema.fromJsonString(Schema.Struct({
  cluster: Schema.String,
  issued_at: Schema.Number,
  expires_at: Schema.Number,
  signer: Schema.Struct({ browser: Schema.String }),
}));
const DeployBodyTerms = Schema.fromJsonString(Schema.Struct({ operation: Schema.String, namespace: Schema.String, machines: Schema.Array(Schema.String) }));
const CommitBody = Schema.fromJsonString(Schema.Struct({ service: Schema.String, commit: Schema.String, rule: Schema.String, generation: Schema.Number }));

/** The terms a body shares with every body of its draft, read back from its own bytes. */
async function sharedLines(body: string): Promise<string[]> {
  const terms = Schema.decodeUnknownSync(SharedTerms)(body);
  return [
    `  cluster ${shownText(terms.cluster)}`,
    `  signer browser key ${shownText(await keyCode(fromHex(terms.signer.browser)))}`,
    `  valid ${shownText(utc(terms.issued_at))} to ${shownText(utc(terms.expires_at))}`,
  ];
}

/** A deploy approval's terms besides its Services, from its own bytes: the Deployment, the Servers, then the shared ones. */
async function deployTermLines(body: string): Promise<string[]> {
  const terms = Schema.decodeUnknownSync(DeployBodyTerms)(body);
  return [
    `  deployment ${shownText(terms.operation)}`,
    `  namespace ${shownText(terms.namespace)}`,
    `  servers ${terms.machines.map(shownText).join(", ")}`,
    ...(await sharedLines(body)),
  ];
}

/**
 * Deployment `deployment`'s approval in `scope` as this browser would sign it now, under the session's browser key for
 * its full 15 minutes, before it signs: the body built from `plan` by the ployz-core code the CLI and the Servers run,
 * so Cloud computes nothing the key signs, and every term it holds. Refused for a plan of another Environment.
 */
export async function draftDeploy(scope: ApprovalScope, deployment: string, plan: DeployPlan, now = Date.now()): Promise<Drafted> {
  const { publicKey } = signer(scope, ["deploy"], [], now);
  if (plan.namespace !== scope.namespace) {
    throw new SessionRefused("The Environment changed after this review. Review the latest changes and try again.");
  }
  const terms = {
    cluster: scope.cluster,
    operation: deployment.replaceAll("-", ""),
    machines: [...new Set(scope.machines)].sort(),
    issued_at: now,
    expires_at: now + APPROVAL_MS,
    signer: { browser: publicKey },
  };
  let drafted;
  try {
    // SAFETY: the terms are the browser's own, and `deployApprovalBody` parses them as ployz-core's `DeployTerms`.
    drafted = deployApprovalBody(plan, terms as DeployTerms);
  } catch (error) {
    throw new SessionRefused(error instanceof Error ? error.message : "This browser couldn't approve the deploy. Try again.");
  }
  return { bodies: [drafted.body], lines: [...drafted.lines, ...(await deployTermLines(drafted.body))], signer: publicKey, expiresAt: terms.expires_at };
}

/** A redeploy's approval of each Service's commit, under the Environment's automatic deploy rule as Cloud relays it. */
export type CommitTerms = {
  readonly rule: string;
  readonly generation: number;
  readonly commits: readonly { readonly service: string; readonly commit: string }[];
};

/**
 * A `CommitApproval` per Service of a redeploy in `scope`, as this browser would sign it now under the session's browser
 * key for its full 15 minutes, before it signs, and every term the bodies hold, read back from them as `ployz deployment
 * redeploy` prints its own. The rule and generation are Cloud's, echoed for each Server to compare with its own.
 */
export async function draftCommits(scope: ApprovalScope, terms: CommitTerms, now = Date.now()): Promise<Drafted> {
  const { publicKey } = signer(scope, ["deploy"], [], now);
  const bodies = terms.commits.map(({ service, commit }) => canonicalJson({
    cluster: scope.cluster,
    commit,
    expires_at: now + APPROVAL_MS,
    generation: terms.generation,
    issued_at: now,
    rule: terms.rule,
    service,
    signer: { browser: publicKey },
  }));
  const opened = bodies.map((body) => Schema.decodeUnknownSync(CommitBody)(body));
  const [first] = opened;
  if (first === undefined || bodies[0] === undefined) throw new SessionRefused("This redeploy built nothing from a commit. Nothing deployed.");
  const lines = [
    "Approving commits for this redeploy:",
    ...opened.map((body) => `  ${shownText(body.service)} at ${shownText(body.commit)}`),
    `  rule ${shownText(first.rule)}, generation ${shownText(String(first.generation))}`,
    ...(await sharedLines(bodies[0])),
  ];
  return { bodies, lines, signer: publicKey, expiresAt: now + APPROVAL_MS };
}

/**
 * Exactly `drafted`'s bodies, each signed by the session's browser key with its grant beside it. Refused once the
 * session holds another key, or once the approval expired, as when the person answered late: nothing is signed then.
 */
export async function signDrafted(drafted: Drafted, now = Date.now()): Promise<Signed[]> {
  const live = current;
  if (live === null || live.publicKey !== drafted.signer) throw new SessionRefused("Confirm it's you again.");
  if (now >= drafted.expiresAt) throw new SessionRefused("Your approval expired before you answered. Nothing deployed.");
  return Promise.all(drafted.bodies.map(async (body) => {
    const signature = hex(await crypto.subtle.sign({ name: "Ed25519" }, live.key.privateKey, signingInput(body)));
    return { body, signatures: [{ key: live.publicKey, signature }], grants: [live.grant] } satisfies Signed;
  }));
}

/**
 * Deployment `deployment`'s approval in `scope`, drafted from `plan` and signed at once: Deploy's own, after the review
 * the person pressed it on. Refused, before anything is signed, for a plan of another version or Environment.
 */
export async function approveDeploy(scope: ApprovalScope, deployment: string, version: string, plan: DeployPlan, now = Date.now()): Promise<DeployApproved> {
  const sent = storeVersion(version);
  if (sent !== plan.version && !sent.startsWith(`${plan.version}:`)) {
    throw new SessionRefused("The Environment changed after this review. Review the latest changes and try again.");
  }
  const drafted = await draftDeploy(scope, deployment, plan, now);
  const [approval] = await signDrafted(drafted, now);
  if (approval === undefined) throw new SessionRefused("This browser couldn't approve the deploy. Try again.");
  return { approval, lines: drafted.lines };
}

export type ScopeMachine = {
  readonly id: string;
  readonly sealing: { readonly kind: string; readonly public_key?: string } | null;
  readonly capabilities: readonly string[];
};

const heldKey = (machine: ScopeMachine) => machine.sealing?.kind === "held" && machine.sealing.public_key ? machine.sealing.public_key : undefined;

/** Every Server holding `cluster`'s Sealing Key. */
const clusterMachines = <M extends ScopeMachine>(cluster: string, machines: readonly M[]) => machines.filter((machine) => heldKey(machine) === cluster);

/**
 * The Cluster the Servers `near` hold the Sealing Key of, or the only one, and every Server in it; every Server when
 * Cloud can't tell which Cluster it is.
 */
export function clusterOf<M extends ScopeMachine>(near: readonly string[], machines: readonly M[]) {
  const running = new Set(near);
  const keys = new Set(machines.map(heldKey).filter((key) => key !== undefined));
  const cluster = machines.filter((machine) => running.has(machine.id)).map(heldKey).find((key) => key !== undefined)
    ?? (keys.size === 1 ? [...keys][0] : undefined);
  return { cluster, members: cluster === undefined ? machines : clusterMachines(cluster, machines) };
}

/**
 * The scope a press on these copies asks a passkey for: the Cluster the copies' Servers hold the Sealing Key of, and every
 * Server in it, so the session also covers a deploy, which may place on any of them. Null when Cloud can't tell which
 * Cluster it is.
 */
type ScopeEnvironment = { readonly environmentId: string; readonly environmentName: string; readonly namespace: string };

export function actScope(environment: ScopeEnvironment, copies: readonly { readonly machineId: string }[], machines: readonly ScopeMachine[]): ApprovalScope | null {
  const hosting = new Set(copies.map((copy) => copy.machineId));
  const cluster = machines.filter((machine) => hosting.has(machine.id)).map(heldKey).find((key) => key !== undefined);
  if (cluster === undefined) return null;
  const members = clusterMachines(cluster, machines).map((machine) => machine.id);
  return { ...environment, cluster, machines: [...new Set([...members, ...hosting])].sort() };
}

/**
 * The scope a Deploy of the Environment's approval names, and the first Server that can't take one from a browser. The
 * Cluster is the one the Servers running the Environment hold the Sealing Key of, or the only one; the scope names every
 * Server in it, since a Deploy may place on any of them. An approval goes only when every Server takes it: one that
 * can't would refuse the Deploy. An automatic deploy rule's scope is the same, with the rule's `capabilities`.
 */
export type DeployScope = { readonly scope: ApprovalScope | null; readonly older: OlderServer | null };

export function deployScope(
  environment: ScopeEnvironment,
  environmentMachines: readonly string[],
  machines: readonly (ScopeMachine & { readonly name: string; readonly capabilities: readonly string[] })[],
  capabilities: readonly string[] = DEPLOY_CAPABILITIES,
): DeployScope {
  const { cluster, members } = clusterOf(environmentMachines, machines);
  return {
    scope: cluster === undefined ? null : { ...environment, cluster, machines: members.map((machine) => machine.id).sort() },
    older: olderServer(members, capabilities),
  };
}
