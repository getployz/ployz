import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { expect, it } from "vitest";
import { BuildLogs, clock, formatDuration, splitStepName, stripAnsi, type BuildOutputRow, type BuildStepRow } from "./deployment-logs";

const deploymentId = "8f79e99b-cd08-4e9c-af96-f3fed313acc5";
const at = (seconds: number) => new Date(Date.UTC(2026, 8, 22, 21, 9, seconds));
const step = (id: number, name: string, extra: Partial<BuildStepRow> = {}): BuildStepRow => ({
  id, deploymentId, key: `sha256:${id}`, name, startedAt: at(id), completedAt: null, cached: false, error: null, createdAt: at(id), updatedAt: at(id), ...extra,
});
const line = (id: number, stepId: number, text: string, stderr = false): BuildOutputRow => ({ id, deploymentId, stepId, stderr, text, createdAt: at(id) });

it("renders started steps as rows, tails the running step, and opens only the failed one", () => {
  const steps = [
    step(1, "[internal] load .dockerignore", { completedAt: at(1), cached: true }),
    step(2, "[sdk 4/6] COPY core/ core/", { completedAt: at(3) }),
    step(3, "[sdk 5/6] RUN bash core/scripts/build-cloud-sdk.sh"),
    step(4, "[node 1/3] RUN npm ci", { completedAt: at(5), error: "process \"/bin/sh -c npm ci\" did not complete successfully: exit code: 1" }),
    step(5, "[go 1/1] FROM golang", { startedAt: null }),
  ];
  const output = [line(1, 2, "<script>alert(1)</script>\n"), line(2, 3, "\u001b[32mCompiling\u001b[0m ployz-core\n   Compiling ployz\n"), line(3, 4, "npm ERR! missing script\n", true)];
  const html = renderToStaticMarkup(createElement(BuildLogs, { hasBuild: true, finished: false, steps, output, now: at(9).getTime() }));
  expect(html.match(/<li/g)).toHaveLength(4);
  expect(html).toContain("cached");
  expect(html).toContain("&lt;script&gt;");
  expect(html).not.toContain("<script>");
  expect(html.match(/<details>/g)).toHaveLength(2);
  expect(html.match(/<details open="">/g)).toHaveLength(1);
  // The running step shows only its last line beneath a collapsed row, colour codes stripped.
  expect(html).toContain(">   Compiling ployz</pre>");
  expect(html).not.toContain("[32m");
  expect(html).toContain("exit code: 1");
  expect(html).toContain("border-destructive");
  expect(html).toContain(clock(at(3)));
  expect(html).toContain(">0ms<");
  expect(html).toContain(">1s<");
  expect(html).toContain(">6s<");
});

it("names the empty states", () => {
  const render = (hasBuild: boolean, finished: boolean) => renderToStaticMarkup(createElement(BuildLogs, { steps: [], output: [], hasBuild, finished }));
  expect(render(true, true)).toContain("No retained build output");
  expect(render(true, false)).toContain("Waiting for the build to start");
  expect(render(false, true)).toContain("uses prebuilt images");
});

it("splits BuildKit step names, rounds durations like Railway, and strips terminal sequences", () => {
  expect(splitStepName("[sdk 4/6] COPY core/ core/")).toEqual({ stage: "sdk", title: "COPY core/ core/" });
  expect(splitStepName("[internal] load build definition from Dockerfile")).toEqual({ stage: "internal", title: "load build definition from Dockerfile" });
  expect(splitStepName("[2/3] RUN echo hi")).toEqual({ stage: null, title: "RUN echo hi" });
  expect(splitStepName("Uploading source")).toEqual({ stage: null, title: "Uploading source" });
  expect([97, 1_234, 7_600, 142_500].map(formatDuration)).toEqual(["97ms", "1s", "7s", "2m 22s"]);
  expect(stripAnsi("\u001b[1;31mBuild failed\u001b[0m\u001b]0;title\u0007 done")).toBe("Build failed done");
});
