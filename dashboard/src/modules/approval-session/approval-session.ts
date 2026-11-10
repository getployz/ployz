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
import type { JsonValue } from "#/db/tables";
import { asBoolean, asFiniteNumber, asRecord, asString } from "#/lib/json";
import { storeVersion } from "#/modules/volume-placement/volume-placement";

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

/** The bytes a key signs, and a grant's challenge hashes: `ployz-authority/v1`, the purpose, and the body. */
export const signingInput = (body: string) => encoder.encode(`ployz-authority/v1\nworkflow\n${body}`);

export const hex = (bytes: ArrayBuffer | Uint8Array) =>
  Array.from(bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes), (byte) => byte.toString(16).padStart(2, "0")).join("");

export const base64url = (bytes: ArrayBuffer | Uint8Array) => {
  let text = "";
  for (const byte of bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes)) text += String.fromCharCode(byte);
  return btoa(text).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
};

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

export const grantChallenge = async (body: string) => new Uint8Array(await crypto.subtle.digest("SHA-256", signingInput(body)));

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
  // SAFETY: a `publicKey` get request answers with a public key credential, or null.
  const credential = await navigator.credentials.get({
    publicKey: { challenge: await grantChallenge(body), rpId: location.hostname, userVerification: "required", timeout: 60_000 },
  }) as PublicKeyCredential | null;
  if (credential === null) throw new SessionRefused("Your passkey didn't answer. Try again.");
  // SAFETY: a `publicKey` get request answers with an assertion response.
  const response = credential.response as AuthenticatorAssertionResponse;
  const grant: SessionGrant = {
    body,
    assertion: {
      credential_id: base64url(credential.rawId),
      authenticator_data: base64url(response.authenticatorData),
      client_data_json: base64url(response.clientDataJSON),
      signature: base64url(response.signature),
    },
  };
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

/** A Store plan read with `target`, as the browser signs a Deploy of it. */
export type DeployPlan = Pick<PlanView, "version" | "namespace" | "target">;

/** What a Deploy's approval holds, and what it shows the person: one `Approved web: nginx@sha256:…` line per Service. */
export type DeployApproved = { readonly approval: Signed; readonly lines: readonly string[] };

/**
 * Deployment `deployment`'s approval in `scope`, signed by the session's browser key and lasting its full 15 minutes.
 * The browser builds the body itself from `plan`, the plan it read at `version`, the version of the review the person
 * pressed Deploy on: each Service's digest is taken by the ployz-core code the CLI and the Servers run, so Cloud
 * computes nothing the key signs. Refused, before anything is signed, for a plan of another version or Environment.
 */
export async function approveDeploy(scope: ApprovalScope, deployment: string, version: string, plan: DeployPlan, now = Date.now()): Promise<DeployApproved> {
  const { grant, publicKey, key } = signer(scope, ["deploy"], [], now);
  const sent = storeVersion(version);
  if (plan.namespace !== scope.namespace || (sent !== plan.version && !sent.startsWith(`${plan.version}:`))) {
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
  const signature = hex(await crypto.subtle.sign({ name: "Ed25519" }, key.privateKey, signingInput(drafted.body)));
  return { approval: { body: drafted.body, signatures: [{ key: publicKey, signature }], grants: [grant] }, lines: drafted.lines };
}

type ScopeMachine = {
  readonly id: string;
  readonly sealing: { readonly kind: string; readonly public_key?: string } | null;
  readonly deviceEnforcement?: boolean | null;
};

const heldKey = (machine: ScopeMachine) => machine.sealing?.kind === "held" && machine.sealing.public_key ? machine.sealing.public_key : undefined;

/** Every Server holding `cluster`'s Sealing Key. */
const clusterMachines = <M extends ScopeMachine>(cluster: string, machines: readonly M[]) => machines.filter((machine) => heldKey(machine) === cluster);

/**
 * Whether a press on these copies needs an approval, and the scope its grant names: the Cluster the copies' Servers hold
 * the Sealing Key of, and every Server in it, so the session also covers a deploy, which may place on any of them. A
 * Server that never reported enforcement asks nothing, since its own check is the one that counts.
 */
export type ActScope = { readonly enforced: boolean; readonly scope: ApprovalScope | null };

type ScopeEnvironment = { readonly environmentId: string; readonly environmentName: string; readonly namespace: string };

export function actScope(environment: ScopeEnvironment, copies: readonly { readonly machineId: string }[], machines: readonly ScopeMachine[]): ActScope {
  const hosting = new Set(copies.map((copy) => copy.machineId));
  const hosts = machines.filter((machine) => hosting.has(machine.id));
  const enforced = hosts.some((machine) => machine.deviceEnforcement === true);
  const cluster = hosts.map(heldKey).find((key) => key !== undefined);
  if (cluster === undefined) return { enforced, scope: null };
  const members = clusterMachines(cluster, machines).map((machine) => machine.id);
  return { enforced, scope: { ...environment, cluster, machines: [...new Set([...members, ...hosting])].sort() } };
}

/**
 * Whether a Deploy of the Environment needs an approval, the scope its grant names, and the first Server that enforces
 * but can't take one from a browser. The Cluster is the one the Servers running the Environment hold the Sealing Key of,
 * or the only one; the scope names every Server in it, since a Deploy may place on any of them. An approval goes only
 * when every Server that enforces takes it: one that can't would refuse the Deploy.
 */
export type DeployScope = { readonly enforced: boolean; readonly scope: ApprovalScope | null; readonly older: string | null };

export function deployScope(
  environment: ScopeEnvironment,
  environmentMachines: readonly string[],
  machines: readonly (ScopeMachine & { readonly name: string; readonly capabilities: readonly string[] })[],
): DeployScope {
  const running = new Set(environmentMachines);
  const keys = new Set(machines.map(heldKey).filter((key) => key !== undefined));
  const cluster = machines.filter((machine) => running.has(machine.id)).map(heldKey).find((key) => key !== undefined)
    ?? (keys.size === 1 ? [...keys][0] : undefined);
  const members = cluster === undefined ? machines : clusterMachines(cluster, machines);
  const enforcing = members.filter((machine) => machine.deviceEnforcement === true);
  const older = enforcing.find((machine) => !DEPLOY_CAPABILITIES.every((capability) => machine.capabilities.includes(capability)))?.name ?? null;
  return {
    enforced: enforcing.length > 0,
    scope: cluster === undefined ? null : { ...environment, cluster, machines: members.map((machine) => machine.id).sort() },
    older,
  };
}
