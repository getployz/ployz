import { stdoutAudit, type AuditSink } from "./audit.js";
import { Github } from "./github.js";
import { CANONICAL_DIGESTS } from "./policy.js";
import { createMinter, type MinterConfig } from "./routes.js";
import { KmsSigner, type Signer } from "./signer.js";

/** The parts of a Lambda Function URL event the minter reads. */
export interface FunctionUrlEvent {
  rawPath: string;
  body?: string;
  isBase64Encoded?: boolean;
  requestContext: { http: { method: string }; authorizer?: { iam?: { userArn?: string } } };
}

export interface FunctionUrlResult {
  statusCode: number;
  headers: Record<string, string>;
  body: string;
}

/**
 * The deployed minter, behind a Function URL with `AWS_IAM` auth. It signs only through KMS and dispatches only the
 * canonical templates built into it. A request that IAM authenticated no one for is refused, so a Function URL left
 * on `NONE` serves nothing.
 */
export function createLambdaHandler(options: {
  signer: Signer;
  config: Omit<MinterConfig, "canonical">;
  audit?: AuditSink;
  fetch?: typeof fetch;
}) {
  if (!(options.signer instanceof KmsSigner)) throw new Error("The Lambda minter signs only with KMS.");
  const github = new Github(options.signer, options.config.appId, { api: "https://api.github.com", web: "https://github.com" }, options.fetch);
  const handle = createMinter({ github, config: { ...options.config, canonical: CANONICAL_DIGESTS }, audit: options.audit ?? stdoutAudit });
  return async (event: FunctionUrlEvent): Promise<FunctionUrlResult> => {
    const response = await handle({
      method: event.requestContext.http.method,
      path: event.rawPath,
      caller: event.requestContext.authorizer?.iam?.userArn ?? null,
      body: parseBody(event.body === undefined ? undefined : event.isBase64Encoded ? Buffer.from(event.body, "base64").toString("utf8") : event.body),
    });
    return { statusCode: response.status, headers: { "Content-Type": "application/json" }, body: JSON.stringify(response.body) };
  };
}

export function parseBody(raw: string | undefined): unknown {
  if (raw === undefined) return undefined;
  try {
    return JSON.parse(raw);
  } catch {
    return undefined;
  }
}

function env(name: string): string {
  const value = process.env[name];
  if (!value) throw new Error(`${name} is not set.`);
  return value;
}

let deployed: ReturnType<typeof createLambdaHandler> | undefined;

export const handler = (event: FunctionUrlEvent) => {
  deployed ??= createLambdaHandler({
    signer: new KmsSigner(env("MINTER_KMS_KEY_ID")),
    config: {
      appId: env("GITHUB_APP_ID"),
      clientId: env("GITHUB_APP_CLIENT_ID"),
      clientSecret: env("GITHUB_APP_CLIENT_SECRET"),
      cloud: new URL(env("MINTER_CLOUD_ORIGIN")).origin,
    },
  });
  return deployed(event);
};
