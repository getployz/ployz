import net, { BlockList, isIP } from "node:net";
import { Option, Schema } from "effect";

const loopback = new BlockList();
loopback.addSubnet("127.0.0.0", 8, "ipv4");
loopback.addAddress("::1", "ipv6");

const HOSTED_DNS = "dns.ployz.app";
/** The model APIs a live agent trial calls. Only `pnpm eval:live`, which sets PLOYZ_EVAL_LIVE=1, may reach them. */
const LIVE_EVAL_HOSTS = new Set(["api.anthropic.com", "api.openai.com"]);

const normalizeHost = (host: string) => host.toLowerCase().replace(/^\[(.*)\]$/, "$1").replace(/\.$/, "");

/** Whether a test may connect to `host`: loopback always, the live eval's model APIs only when opted in. */
export function allowsHost(host: string, env: NodeJS.ProcessEnv = process.env) {
  const name = normalizeHost(host);
  const family = isIP(name);
  if (family !== 0) return !name.includes("%") && loopback.check(name, family === 4 ? "ipv4" : "ipv6");
  return name === "localhost" || name.endsWith(".localhost") || (env["PLOYZ_EVAL_LIVE"] === "1" && LIVE_EVAL_HOSTS.has(name));
}

const OptionalString = Schema.optional(Schema.NullishOr(Schema.String));
const decodeOptions = Schema.decodeUnknownOption(Schema.Struct({ host: OptionalString, path: OptionalString }));
const isString = Schema.is(Schema.String);
const isPortOrPath = Schema.is(Schema.Union([Schema.Number, Schema.String]));

/**
 * The host of a TCP connect, null for a Unix socket, or undefined when the arguments can't be read. Node's own callers pass
 * their normalized arguments as one array, and http passes `path: null`.
 */
function tcpHost(args: ReadonlyArray<unknown>) {
  const [first, second] = Array.isArray(args[0]) ? args[0] : args;
  if (isPortOrPath(first)) {
    if (Number.isNaN(Number(first))) return null;
    return isString(second) && second ? second : "localhost";
  }
  return Option.match(decodeOptions(first), {
    onNone: () => undefined,
    onSome: (options) => (options.path ? null : options.host || "localhost"),
  });
}

function egressError(host: string | undefined) {
  const name = host === undefined ? "" : normalizeHost(host);
  const what = name === HOSTED_DNS || name.endsWith(`.${HOSTED_DNS}`) ? "the real Hosted DNS" : "the network";
  const target = host ?? "unreadable connect arguments";
  return Object.assign(new Error(`Tests must not reach ${what}: ${target}. Only loopback is allowed.`), { code: "ERR_TEST_EGRESS" });
}

const GUARDED = Symbol.for("ployz.test.egressGuard");

/**
 * Refuses every TCP connect a test makes to a host other than loopback, before DNS or the connect. fetch, http, https,
 * tls, WebSocket, pg and the SDKs built on them all connect through `net.Socket`.
 */
export function installEgressGuard() {
  if (GUARDED in net.Socket.prototype) return;
  type Connect = (this: net.Socket, ...args: Array<unknown>) => net.Socket;
  // SAFETY: the guard hands connect its caller's arguments untouched, so they still fit one of its overloads.
  const connect = net.Socket.prototype.connect as Connect;
  net.Socket.prototype.connect = function (this: net.Socket, ...args: Array<unknown>) {
    const host = tcpHost(args);
    if (host === null || (host !== undefined && allowsHost(host))) return connect.apply(this, args);
    const error = egressError(host);
    process.nextTick(() => this.destroy(error));
    return this;
  };
  Object.defineProperty(net.Socket.prototype, GUARDED, { value: true });
}
