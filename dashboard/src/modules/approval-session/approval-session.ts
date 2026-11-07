/**
 * A browser approval session. One passkey prompt lends a short-lived browser key the right to approve restarts in one
 * Environment for an hour. Each press then carries one approval per container, signed by that key, beside the passkey's
 * grant of it. Servers check the chain against the passkeys they trust; Cloud only carries it and never holds the key.
 *
 * The shapes and bytes follow `ployz_core::authority`: canonical JSON, the `ployz-authority/v1` signing input, and the
 * grant challenge `SHA-256(signing input of the grant body)`.
 */
import type { ActionFamily, SessionGrant, Signed } from "@ployz/sdk";
import type { JsonValue } from "#/db/tables";
import { asBoolean, asFiniteNumber, asRecord, asString } from "#/lib/json";

/** How long one passkey prompt lasts. Servers cap a grant at an hour. */
export const SESSION_MINUTES = 60;
/**
 * How long one approval lasts. Servers cap it at `validity::MAX_LIFETIME_MILLIS`. A session with less than this left
 * prompts again, so every approval gets its whole window inside the grant and a rolling restart can't outlive it.
 */
const APPROVAL_MS = 15 * 60_000;

export const SESSION_FAMILIES = ["restart"] as const satisfies readonly ActionFamily[];

/** Where a press acts: the Environment, and the Cluster and Servers its grant must name. */
export type ApprovalScope = {
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

export type PromptReason = "first" | "expired" | "scope";

let current: { readonly session: ApprovalSession; readonly grant: SessionGrant; readonly publicKey: string; readonly key: CryptoKeyPair } | null = null;
const listeners = new Set<() => void>();

export function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => void listeners.delete(listener);
}

export const readSession = (): ApprovalSession | null => current?.session ?? null;

const covers = (session: ApprovalSession, scope: ApprovalScope) =>
  session.scope.environmentId === scope.environmentId
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
    expires_at: issuedAt + SESSION_MINUTES * 60_000,
    families: [...SESSION_FAMILIES].sort(),
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
 * One passkey prompt. The grant body names the browser key, the Environment's Cluster and Servers, the actions and the
 * hour; the passkey answers its digest, so the grant covers that body and nothing else.
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

export type RestartCopy = { readonly machine: string; readonly container: string };

const operationId = () => hex(crypto.getRandomValues(new Uint8Array(16)));

/**
 * One approval per container for a restart of `service`, each signed by the session's browser key and lasting its full
 * 15 minutes, which the session still covers. Keyed by container ID, as Cloud forwards them.
 */
export async function approveRestart(service: string, copies: readonly RestartCopy[], now = Date.now()): Promise<Record<string, Signed>> {
  if (current === null || current.session.expiresAt - now < APPROVAL_MS) throw new SessionRefused("Confirm it's you again.");
  const { session, grant, publicKey, key } = current;
  const entries = await Promise.all(copies.map(async ({ machine, container }) => {
    const body = canonicalJson({
      cluster: session.scope.cluster,
      container,
      expires_at: now + APPROVAL_MS,
      issued_at: now,
      kind: "restart",
      machines: [machine],
      namespace: session.scope.namespace,
      operation: operationId(),
      service,
      signer: { browser: publicKey },
    });
    const signature = hex(await crypto.subtle.sign({ name: "Ed25519" }, key.privateKey, signingInput(body)));
    return [container, { body, signatures: [{ key: publicKey, signature }], grants: [grant] } satisfies Signed] as const;
  }));
  return Object.fromEntries(entries);
}

type ScopeMachine = {
  readonly id: string;
  readonly sealing: { readonly kind: string; readonly public_key?: string } | null;
  readonly deviceEnforcement?: boolean | null;
};

/**
 * Whether a press on these copies needs an approval, and the scope its grant names: the Cluster the copies' Servers hold
 * the Sealing Key of, and the Servers in that Cluster that run the Environment. A Server that never reported enforcement
 * asks nothing, since its own check is the one that counts.
 */
export type RestartScope = { readonly enforced: boolean; readonly scope: ApprovalScope | null };

export function restartScope(
  environment: { readonly environmentId: string; readonly environmentName: string; readonly namespace: string },
  copies: readonly { readonly machineId: string }[],
  machines: readonly ScopeMachine[],
  environmentMachines: readonly string[],
): RestartScope {
  const hosting = new Set(copies.map((copy) => copy.machineId));
  const hosts = machines.filter((machine) => hosting.has(machine.id));
  const enforced = hosts.some((machine) => machine.deviceEnforcement === true);
  const cluster = hosts.flatMap((machine) => machine.sealing?.kind === "held" && machine.sealing.public_key ? [machine.sealing.public_key] : [])[0];
  if (cluster === undefined) return { enforced, scope: null };
  const running = new Set(environmentMachines);
  const members = machines
    .filter((machine) => running.has(machine.id) && machine.sealing?.kind === "held" && machine.sealing.public_key === cluster)
    .map((machine) => machine.id);
  return { enforced, scope: { ...environment, cluster, machines: [...new Set([...members, ...hosting])].sort() } };
}
