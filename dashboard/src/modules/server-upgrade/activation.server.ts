import "@tanstack/react-start/server-only";
import type { MachineDetails, OperationId, SetRecipientKeyRequest, Signed } from "@ployz/sdk";
import { Effect, Schema } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { requireOperator } from "#/modules/ingress/doors.access.server";
import { currentGeneration } from "#/modules/machines/connections.server";
import { recordSealingRecipient } from "#/modules/machines/sealing-key-read.server";
import { refusalWords } from "#/modules/machines/server-web-traffic.server";
import { OrganizationRuntime, RUNTIME_FRAME_TIMEOUT_MS } from "#/modules/runtime/organization-runtime.server";
import { ServerLacks } from "#/modules/runtime/daemon-compat";
import { type PloyzSdkError, type PloyzSession, sdkFailureMessage } from "#/modules/runtime/ployz.server";
import { Validation } from "#/server/public-error";
import {
  type ActivationServer,
  ENFORCED_CAPABILITY,
  type Generated,
  type InstallRecipientInput,
  type PasskeyAnswered,
  type RecipientInstall,
  RotationRead,
  type ServerActivation,
  StartingSetRead,
} from "./activation";

/** How long one Server's Inspect may take before it reads as not answering. */
const INSPECT_LIMIT_MS = 10_000;
/** How often, and how many times, Cloud asks a Server whether it came back from its activation restart. */
const SETTLE_EVERY = "1 second";
const SETTLE_LOOKS = 60;

const UNANSWERED = "Your servers aren't answering. Try again once they are.";
const refuse = (message: string) => new Validation({ message, userFacing: true });

/** One Server as Cloud read it: its own Inspect when it answered, else only its name. */
export function activationServer(id: string, name: string, details: MachineDetails | null): ActivationServer {
  const machine = details?.machine?.id === id ? details.machine : null;
  const report = details?.activation ?? null;
  return {
    id,
    name,
    answered: machine !== null,
    enforces: machine?.runtime.capabilities?.includes(ENFORCED_CAPABILITY) ?? false,
    member: machine === null ? null : { id: machine.id, public_key: [...machine.public_key], subnet: machine.subnet },
    identity: details?.sealing?.kind === "held" ? details.sealing.public_key : null,
    recipient: details?.sealing_recipient ?? null,
    activation: report === null ? null : {
      state: report.state,
      operation: report.operation ?? null,
      legacyMembers: [...report.legacy_members],
      legacyKeys: report.legacy_keys.map((key) => ({ code: key.code, kind: key.kind, account: key.account ?? null })),
      privateAccess: report.private_access === undefined || report.private_access === null
        ? null
        : { tailnet: report.private_access.tailnet ?? null },
    },
  };
}

const connected = Effect.fn("Activation.session")(function* (organizationId: string) {
  const opened = yield* (yield* OrganizationRuntime).open(organizationId);
  return opened.status === "connected" ? opened.connected : yield* refuse(UNANSWERED);
});

/**
 * `use` on a session of its own. A Server restarts to activate and its connection dies with it, and the entry may be
 * that Server, so each step after an activation dials again.
 */
const freshly = <A, E, R>(organizationId: string, use: (session: PloyzSession) => Effect.Effect<A, E, R>) =>
  Effect.scoped(Effect.flatMap(connected(organizationId), use));

const inspected = (session: PloyzSession, id: string) => session.inspectMachine(id).pipe(
  Effect.timeoutOrElse({ duration: INSPECT_LIMIT_MS, orElse: () => Effect.succeed(null) }),
  Effect.orElseSucceed(() => null),
);

/** Each Server the Cluster lists, by name, as the entry's Runtime Watch reports them. */
const listed = (session: PloyzSession) => session.watchFirstFrame(RUNTIME_FRAME_TIMEOUT_MS).pipe(
  Effect.map((frame) => frame.machines.map(({ machine }) => ({ id: machine.id, name: machine.name }))),
  Effect.mapError(() => refuse(UNANSWERED)),
);

/**
 * Keep the recipient key the Servers report for their identity, so values sealed from now on go to it. Each Server's
 * own report is the evidence, and it never goes back once kept.
 */
const keepRecipient = Effect.fn("Activation.keepRecipient")(function* (organizationId: string, servers: readonly ActivationServer[]) {
  const reported = servers.find((server) => server.identity !== null && server.recipient !== null);
  if (reported?.identity == null || reported.recipient === null) return;
  const generation = yield* currentGeneration(organizationId);
  if (generation === null) return;
  yield* recordSealingRecipient(organizationId, generation, reported.identity, reported.recipient);
});

/** Every Server's own account of its activation, read through Inspect one by one. */
export const readActivation = Effect.fn("Activation.read")(function* (organizationId: string) {
  const session = yield* connected(organizationId);
  const machines = yield* listed(session);
  const servers = yield* Effect.forEach(machines, ({ id, name }) =>
    inspected(session, id).pipe(Effect.map((details) => activationServer(id, name, details))), { concurrency: "unbounded" });
  yield* keepRecipient(organizationId, servers);
  return servers;
}, Effect.scoped);

/** A Server's refusal as it said it, or which Server needs a newer Ployz. */
const words = (error: PloyzSdkError, servers: readonly { readonly id: string; readonly name: string }[] = []) =>
  error instanceof ServerLacks ? sdkFailureMessage(error, servers) : refusalWords(error);

/**
 * Wait for Server `id` to come back from its restart and report the activation `operation` committed. A Server that
 * doesn't answer while it restarts is asked again.
 */
const settled = Effect.fn("Activation.settle")(function* (organizationId: string, id: string, name: string, operation: string) {
  for (let look = 0; look < SETTLE_LOOKS; look += 1) {
    const details = yield* freshly(organizationId, (session) => inspected(session, id)).pipe(Effect.orElseSucceed(() => null));
    const report = activationServer(id, name, details).activation;
    if (report?.state === "activated") {
      return report.operation === operation
        ? { id, name, kind: "activated", privateAccess: report.privateAccess } satisfies ServerActivation
        : { id, name, kind: "refused", why: "It was already activated with another list of servers." } satisfies ServerActivation;
    }
    yield* Effect.sleep(SETTLE_EVERY);
  }
  return { id, name, kind: "refused", why: "It didn't report the activation finished. Check it with `ployz status`." } satisfies ServerActivation;
});

// SAFETY: StartingSetRead checks it is 32 lowercase hex characters, the form OperationId parses.
const opId = (operation: string) => operation as OperationId;

/** A passkey's answer as the Servers take it. Cloud bounds its size; each Server parses and verifies every byte. */
const relayed = (answered: PasskeyAnswered): Signed => ({
  body: answered.body,
  signatures: [],
  assertions: answered.assertions.map((assertion) => ({ ...assertion })),
});

const readBody = <A>(schema: Schema.Decoder<A>, answered: PasskeyAnswered, what: string) =>
  Schema.decodeUnknownEffect(schema)(answered.body).pipe(Effect.mapError(() => refuse(`This approval isn't ${what}. Nothing changed.`)));

/**
 * Hand the starting set a passkey answered to each Server it names, one at a time so the Cluster never loses every
 * daemon at once, and wait for each to report it committed. The person's right to operate the Servers is checked again
 * before each one.
 */
export const activateServers = Effect.fn("Activation.activate")(function* (
  actor: Actor, organizationSlug: string, startingSet: PasskeyAnswered,
) {
  const { organization } = yield* requireOperator(actor, organizationSlug);
  const set = yield* readBody(StartingSetRead, startingSet, "a list of servers to activate");
  const names = yield* freshly(organization.id, listed).pipe(
    Effect.map((machines) => new Map<string, string>(machines.map(({ id, name }) => [id, name]))),
  );
  const servers: ServerActivation[] = [];
  for (const id of [...new Set(set.members.map((member) => member.id))].sort()) {
    const name = names.get(id) ?? id;
    yield* requireOperator(actor, organizationSlug);
    const sent = yield* freshly(organization.id, (session) => session.activateEnforcement(id, relayed(startingSet)).pipe(
      Effect.timeoutOrElse({ duration: INSPECT_LIMIT_MS, orElse: () => Effect.succeed("unsent" as const) }),
      Effect.catch((error: PloyzSdkError) => Effect.succeed({ refused: words(error, [{ id, name }]) })),
    ));
    if (sent === "unsent") servers.push({ id, name, kind: "refused", why: "It didn't answer. Check it with `ployz status`." });
    else if ("refused" in sent) servers.push({ id, name, kind: "refused", why: sent.refused });
    else servers.push(yield* settled(organization.id, id, name, set.operation));
  }
  return servers;
}, Effect.scoped);

/**
 * Have the lowest-ID Server that reports activation `operation` committed generate the recipient key a person's second
 * prompt names. Which Servers committed is read from each Server again, not from the browser.
 */
export const generateRecipient = Effect.fn("Activation.generate")(function* (actor: Actor, organizationSlug: string, operation: string) {
  const { organization } = yield* requireOperator(actor, organizationSlug);
  const servers = yield* readActivation(organization.id);
  const generator = servers.filter((server) => server.activation?.state === "activated" && server.activation.operation === operation)
    .map(({ id, name }) => ({ id, name }))
    .sort((left, right) => left.id < right.id ? -1 : left.id > right.id ? 1 : 0)[0];
  if (generator === undefined) return yield* refuse("No server reports this activation finished. Nothing changed.");
  yield* requireOperator(actor, organizationSlug);
  const answer = yield* freshly(organization.id, (session) =>
    session.setRecipientKey(generator.id, { step: "generate", operation: opId(operation) }).pipe(
      Effect.timeoutOrElse({ duration: INSPECT_LIMIT_MS, orElse: () => Effect.fail(refuse(UNANSWERED)) }),
      Effect.mapError((error) => error instanceof Validation ? error : refuse(words(error, [generator]))),
    ));
  if (answer.step !== "generated") return yield* refuse("The server didn't make a new key. Nothing changed.");
  return { generator: generator.id, recipient: answer.recipient } satisfies Generated;
});

/**
 * Install the recipient key a passkey's rotation names: on the Server that generated it from its own copy, then on
 * every other Member the rotation names, from the generator's copy sealed to that Member. Each Server checks the
 * rotation itself. The person's right to operate the Servers is checked again before each effect.
 */
export const installRecipient = Effect.fn("Activation.install")(function* (actor: Actor, input: InstallRecipientInput) {
  const { organization } = yield* requireOperator(actor, input.organizationSlug);
  const rotation = yield* readBody(RotationRead, input.rotation, "a new key for sealed values");
  const ids = [...new Set(rotation.members.map((member) => member.id))].sort();
  if (!ids.includes(input.generator)) return yield* refuse("The server that made the key isn't in this approval. Nothing changed.");
  const session = yield* connected(organization.id);
  const names = new Map<string, string>((yield* listed(session).pipe(Effect.orElseSucceed(() => []))).map(({ id, name }) => [id, name]));
  const signed = relayed(input.rotation);
  const nameOf = (id: string) => names.get(id) ?? id;
  const set = (id: string, request: SetRecipientKeyRequest) => requireOperator(actor, input.organizationSlug).pipe(
    Effect.andThen(session.setRecipientKey(id, request).pipe(
      Effect.mapError((error) => words(error, [{ id, name: nameOf(id) }])),
      Effect.timeoutOrElse({ duration: INSPECT_LIMIT_MS, orElse: () => Effect.fail("It didn't answer. Check it with `ployz status`.") }),
    )),
  );
  const outcome = <E, R>(id: string, done: Effect.Effect<unknown, string | E, R>) => done.pipe(
    Effect.as<RecipientInstall>({ id, name: nameOf(id), kind: "installed" }),
    Effect.catchIf((error): error is string => typeof error === "string",
      (why) => Effect.succeed<RecipientInstall>({ id, name: nameOf(id), kind: "refused", why })),
  );

  const first = yield* outcome(input.generator, set(input.generator, { step: "install", rotation: signed, sealed: null }));
  const installs: RecipientInstall[] = [first];
  for (const id of ids.filter((member) => member !== input.generator)) {
    if (first.kind !== "installed") {
      installs.push({ id, name: names.get(id) ?? id, kind: "refused", why: "The server that made the key didn't install it, so it wasn't shared." });
      continue;
    }
    installs.push(yield* outcome(id, Effect.gen(function* () {
      const shared = yield* set(input.generator, { step: "share", rotation: signed, to: id });
      if (shared.step !== "shared") return yield* Effect.fail("The server that made the key didn't share it.");
      return yield* set(id, { step: "install", rotation: signed, sealed: shared.sealed });
    })));
  }
  const after = yield* inspected(session, input.generator);
  yield* keepRecipient(organization.id, [activationServer(input.generator, names.get(input.generator) ?? input.generator, after)]);
  return installs;
}, Effect.scoped);
