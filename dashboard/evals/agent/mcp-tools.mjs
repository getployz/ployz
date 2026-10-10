import { readFileSync } from "node:fs";
import readline from "node:readline";

const tools = JSON.parse(readFileSync(process.argv[2], "utf8"));
const send = (message) => process.stdout.write(`${JSON.stringify(message)}\n`);

readline.createInterface({ input: process.stdin }).on("line", (line) => {
  const { id, method, params } = JSON.parse(line);
  if (id === undefined || method === "tools/call") return;
  if (method === "initialize") {
    send({ jsonrpc: "2.0", id, result: { protocolVersion: params.protocolVersion, capabilities: { tools: {} }, serverInfo: { name: "ployz", version: "0" } } });
  } else {
    send({ jsonrpc: "2.0", id, result: method === "tools/list" ? { tools } : {} });
  }
});
