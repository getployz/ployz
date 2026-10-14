// The fake PostShiba a MAIL=1 verify lab points the Mailer at. It answers PostShiba's send route and relays each mail
// into Mailpit (MAILPIT_URL), so a lab reads Cloud's mail in Mailpit's inbox and nothing leaves the machine. A repeated
// idempotency-key returns the first message id and relays nothing, as PostShiba does. up.sh runs it with runner.mjs
// until down.sh stops it. Prints `VERIFY_FAKE_POSTSHIBA <url>` once listening, then one line per send.
import { createServer, type IncomingMessage } from "node:http";

type Send = { from: string; to: string[]; subject: string; text: string; html?: string };

const SEND_ROUTE = /^\/api\/v1\/teams\/[^/]+\/clusters\/[^/]+\/sends$/;

const readBody = async (request: IncomingMessage) => {
  const chunks: Buffer[] = [];
  for await (const chunk of request) chunks.push(chunk as Buffer);
  return Buffer.concat(chunks).toString("utf8");
};

const address = (value: string) => {
  const named = /^\s*(.*?)\s*<([^>]+)>\s*$/.exec(value);
  return named ? { Name: named[1] ?? "", Email: named[2] ?? "" } : { Name: "", Email: value.trim() };
};

export async function run() {
  const mailpit = process.env["MAILPIT_URL"];
  if (!mailpit) throw new Error("fake PostShiba needs MAILPIT_URL");
  const sent = new Map<string, string>();

  const server = createServer((request, response) => {
    const reply = (status: number, json: unknown) => {
      response.writeHead(status, { "content-type": "application/json" });
      response.end(JSON.stringify(json));
    };
    void (async () => {
      if (request.method !== "POST" || !SEND_ROUTE.test(new URL(request.url ?? "/", "http://fake").pathname)) {
        return reply(404, { error: "not_found" });
      }
      const key = request.headers["idempotency-key"];
      if (typeof key !== "string" || key === "") return reply(400, { error: "missing_idempotency_key" });
      const body = JSON.parse(await readBody(request)) as Send;
      const known = sent.get(key);
      if (known !== undefined) {
        console.log(`send ${key} repeated, ${known}`);
        return reply(201, { message_id: known });
      }
      const relayed = await fetch(`${mailpit}/api/v1/send`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          From: address(body.from),
          To: body.to.map(address),
          Subject: body.subject,
          Text: body.text,
          HTML: body.html ?? "",
          Headers: { "Idempotency-Key": key },
        }),
      }).catch((cause: unknown) => cause);
      if (!(relayed instanceof Response) || !relayed.ok) {
        console.log(`send ${key} to ${body.to.join(",")} failed: Mailpit unavailable`);
        return reply(503, { error: "inject_unavailable", retryable: true });
      }
      const { ID } = (await relayed.json()) as { ID: string };
      sent.set(key, ID);
      console.log(`send ${key} to ${body.to.join(",")} ${ID}`);
      return reply(201, { message_id: ID });
    })().catch((error: unknown) => reply(500, { error: String(error) }));
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const bound = server.address();
  if (bound === null || typeof bound === "string") throw new Error("fake PostShiba has no port");
  console.log(`VERIFY_FAKE_POSTSHIBA http://127.0.0.1:${bound.port}`);
  await new Promise<never>(() => {});
}
