import { spawn } from "node:child_process";

const port = parsePositiveInteger(process.env["PORT"] ?? "3000", "PORT", 65_535);

const server = spawn(process.execPath, [".output/server/index.mjs"], {
  stdio: "inherit",
  env: process.env,
});

server.on("exit", (code, signal) => {
  if (signal) {
    process.kill(process.pid, signal);
    return;
  }
  process.exit(code ?? 0);
});

for (const signal of ["SIGINT", "SIGTERM"]) {
  process.on(signal, () => {
    server.kill(signal);
  });
}

void syncInngestFunctions();

async function syncInngestFunctions() {
  const endpoint = new URL(
    "/api/inngest",
    `http://127.0.0.1:${port}`,
  );
  const maxAttempts = parsePositiveInteger(
    process.env.INNGEST_SYNC_ATTEMPTS ?? "60",
    "INNGEST_SYNC_ATTEMPTS",
  );
  const delayMs = parsePositiveInteger(
    process.env.INNGEST_SYNC_DELAY_MS ?? "2000",
    "INNGEST_SYNC_DELAY_MS",
  );

  for (let attempt = 1; attempt <= maxAttempts; attempt += 1) {
    try {
      const response = await fetch(endpoint, {
        method: "PUT",
        headers: {
          "content-type": "application/json",
          "x-inngest-server-kind": "cloud",
        },
        body: "{}",
      });
      const body = await response.text();
      if (response.ok) {
        console.log(`Synced Inngest functions: ${body}`);
        return;
      }
      console.warn(
        `Inngest function sync attempt ${attempt}/${maxAttempts} failed: HTTP ${response.status} ${body}`,
      );
    } catch (error) {
      console.warn(
        `Inngest function sync attempt ${attempt}/${maxAttempts} failed: ${
          error instanceof Error ? error.message : String(error)
        }`,
      );
    }

    await sleep(delayMs);
  }

  console.error(`Inngest function sync failed after ${maxAttempts} attempts.`);
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function parsePositiveInteger(value, name, maximum = Number.MAX_SAFE_INTEGER) {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < 1 || parsed > maximum) {
    throw new Error(`${name} must be an integer between 1 and ${maximum}`);
  }
  return parsed;
}
