import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { describe, expect, it } from "vitest";

const upSh = resolve(import.meta.dirname, "../../scripts/verify/up.sh");
const keep = resolve(import.meta.dirname, "../../../.agents/skills/soak/scripts/keep");

/** up.sh variables that hold a cookie, token, password or key. */
const SECRETS = ["cookie", "bob_cookie", "bob_credentials", "key", "secret", "seed"];
const secretReference = new RegExp(`\\$\\{?(?:${SECRETS.join("|")})\\b`);

/**
 * The lines of up.sh that reach the terminal: heredocs not redirected into a file, and echo or printf commands not
 * redirected into one. stderr counts, since agents capture both.
 */
function printedLines(script: string): string[] {
  const printed: string[] = [];
  let heredoc: { end: string; printed: boolean } | null = null;
  for (const line of script.split("\n")) {
    if (heredoc) {
      if (line === heredoc.end) heredoc = null;
      else if (heredoc.printed) printed.push(line);
      continue;
    }
    const start = /<<-?\s*'?(\w+)'?/.exec(line);
    if (start?.[1]) {
      heredoc = { end: start[1], printed: !/\bcat\s*>/.test(line) && !/node -e/.test(line) };
      continue;
    }
    for (const command of line.split(/;|&&|\|\||\{|\}/)) {
      if (/\b(echo|printf)\b/.test(command) && !/>>?\s*"/.test(command)) printed.push(command);
    }
  }
  return printed;
}

describe("up.sh", () => {
  it("prints no cookie, token, password or key", () => {
    const printed = printedLines(readFileSync(upSh, "utf8"));
    expect(printed.some((line) => line.includes("cookies set"))).toBe(true);
    expect(printed.filter((line) => secretReference.test(line))).toEqual([]);
  });

  it("writes its secret files with mode 600", () => {
    const script = readFileSync(upSh, "utf8");
    for (const file of ["env", "seed.json", "cookie", "bob-cookie"]) {
      const writes = script.split("\n").filter((line) => line.includes(`> "$run/${file}"`));
      expect(writes, file).not.toEqual([]);
      for (const write of writes) expect(write, file).toContain("umask 077");
    }
  });
});

describe("soak keep", () => {
  it("keeps a minted token out of stdout and in a mode-600 file", () => {
    const dir = mkdtempSync(join(tmpdir(), "keep-"));
    const file = join(dir, "token.json");
    const token = "plyz_" + "a1b2c3d4".repeat(4);
    const stdout = execFileSync(keep, [file, "--", "printf", `{"token":"${token}","max":3}`], { encoding: "utf8" });
    expect(stdout).not.toContain(token);
    expect(stdout).toContain(`kept: ${file} (mode 600) {"token": "<plyz_37 chars>", "max": 3}`);
    expect(readFileSync(file, "utf8")).toContain(token);
    expect(statSync(file).mode & 0o777).toBe(0o600);
  });
});
