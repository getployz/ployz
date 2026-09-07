import { readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { spawn } from "node:child_process";

const START_MARKER = "<!-- intent-skills:start -->";
const END_MARKER = "<!-- intent-skills:end -->";
const MAX_CONCURRENCY = 100;
let completedCount = 0;

function fail(message) {
  console.error(message);
  process.exit(1);
}

function parseReviewJson(output) {
  const start = output.indexOf("{");
  const end = output.lastIndexOf("}");

  if (start === -1 || end === -1 || end < start) {
    fail("Failed to parse tessl output: no JSON payload found.");
  }

  try {
    return JSON.parse(output.slice(start, end + 1));
  } catch (error) {
    fail(
      `Failed to parse tessl output: ${
        error instanceof Error ? error.message : String(error)
      }`,
    );
  }
}

function runCommand(command, args) {
  return new Promise((resolvePromise, rejectPromise) => {
    const child = spawn(command, args, {
      stdio: ["ignore", "pipe", "pipe"],
    });

    let stdout = "";
    let stderr = "";

    child.stdout.on("data", (chunk) => {
      stdout += chunk.toString();
    });

    child.stderr.on("data", (chunk) => {
      stderr += chunk.toString();
    });

    child.on("error", rejectPromise);
    child.on("close", (code) => {
      resolvePromise({ code, stdout, stderr });
    });
  });
}

async function runTesslReviewJson(skillDir) {
  const review = await runCommand("tessl", [
    "skill",
    "review",
    "--json",
    skillDir,
  ]);
  const output = `${review.stdout ?? ""}${review.stderr ?? ""}`;
  const parsed = parseReviewJson(output);
  const description = parsed?.validation?.skillDescription?.trim();

  if (!description) {
    fail(
      review.stderr ||
        review.stdout ||
        `Missing validation.skillDescription for ${skillDir}`,
    );
  }

  return description;
}

async function optimizeSkillAndReadDescription(skillDir, loadPath, totalCount) {
  console.log(`Optimizing ${loadPath}`);

  await runCommand("tessl", [
    "skill",
    "review",
    "--optimize",
    "--yes",
    skillDir,
  ]);

  const description = await runTesslReviewJson(skillDir);
  completedCount += 1;
  console.log(`Done optimizing (${completedCount}/${totalCount}) ${loadPath}`);
  return description;
}

function escapeDoubleQuotes(value) {
  return value.replaceAll("\\", "\\\\").replaceAll("\"", "\\\"");
}

async function mapWithConcurrency(items, limit, mapper) {
  const results = new Array(items.length);
  let nextIndex = 0;

  async function worker() {
    while (true) {
      const currentIndex = nextIndex;
      nextIndex += 1;

      if (currentIndex >= items.length) {
        return;
      }

      results[currentIndex] = await mapper(items[currentIndex], currentIndex);
    }
  }

  const workerCount = Math.min(limit, items.length);
  await Promise.all(Array.from({ length: workerCount }, () => worker()));
  return results;
}

async function main() {
  const targetFile = resolve(process.argv[2] ?? "AGENTS.md");
  const source = readFileSync(targetFile, "utf8");
  const startIndex = source.indexOf(START_MARKER);
  const endIndex = source.indexOf(END_MARKER);

  if (startIndex === -1 || endIndex === -1 || endIndex <= startIndex) {
    fail(`Could not find intent-skills block in ${targetFile}`);
  }

  const blockStart = startIndex + START_MARKER.length;
  const block = source.slice(blockStart, endIndex);
  const lines = block.split("\n");
  const updatedLines = [...lines];
  const entries = [];

  for (let i = 0; i < lines.length; i += 1) {
    const line = lines[i];
    const loadMatch = lines[i + 1]?.match(/^(\s+load:\s+")(.+)(")$/);

    if (!line.match(/^\s+- task: "/) || !loadMatch) {
      continue;
    }

    const [, loadPrefix, loadPath, loadSuffix] = loadMatch;
    entries.push({
      taskLineIndex: i,
      loadLineIndex: i + 1,
      loadPrefix,
      loadPath,
      loadSuffix,
      skillDir: dirname(resolve(loadPath)),
    });

    i += 1;
  }

  const descriptions = await mapWithConcurrency(
    entries,
    MAX_CONCURRENCY,
    async (entry) =>
      optimizeSkillAndReadDescription(entry.skillDir, entry.loadPath, entries.length),
  );

  for (let i = 0; i < entries.length; i += 1) {
    const entry = entries[i];
    const description = descriptions[i];
    updatedLines[entry.taskLineIndex] = `  - task: "${escapeDoubleQuotes(description)}"`;
    updatedLines[entry.loadLineIndex] = `${entry.loadPrefix}${entry.loadPath}${entry.loadSuffix}`;
  }

  const nextSource =
    source.slice(0, blockStart) +
    updatedLines.join("\n") +
    source.slice(endIndex);

  if (nextSource !== source) {
    writeFileSync(targetFile, nextSource);
    console.log(`Updated ${targetFile}`);
  } else {
    console.log(`No changes in ${targetFile}`);
  }
}

await main();
