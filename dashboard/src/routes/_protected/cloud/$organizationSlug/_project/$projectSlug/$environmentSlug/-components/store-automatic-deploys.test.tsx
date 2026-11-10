// @vitest-environment jsdom

import { useState } from "react";
import type { RuleView, Signed } from "@ployz/sdk";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { draftRule } from "#/modules/github/auto-deploy-rule";
import { RuleDialog } from "./store-automatic-deploys";

const SEALED = "QUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQQ==";
const SCOPE = {
  environmentId: "11111111-1111-1111-1111-111111111111",
  environmentName: "production",
  namespace: "app",
  cluster: "CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=",
  machines: ["1".repeat(32)],
};
const OWNER = { repositoryId: 1_450_985_696, ownerId: 278_111_222 };

const spec = (name: string, plain: string) => ({
  name,
  mode: { mode: "replicated", replicas: 1 },
  container: {
    image: `ployz-build/${name}`,
    pull_policy: "never",
    environment: { PLAIN: plain, SECRET: { sealed: SEALED, scope: "shared", key: "SECRET" } },
    sealed_identity: {
      organization: "org",
      environment: "11111111-1111-1111-1111-111111111111",
      service: "22222222-2222-2222-2222-222222222222",
    },
  },
});

/** The Store's rule view of `web` on `main`, keeping `rule`. */
const view = (rule: RuleView["rule"], web: string): RuleView =>
  // SAFETY: the fixture is the JSON the Store answers; `autoDeployRuleBody` parses what it reads of it.
  JSON.parse(JSON.stringify({
    environment: { id: SCOPE.environmentId },
    namespace: "app",
    rule,
    sources: [{
      service: "web",
      repository: "Ployz-test/lab-service",
      repository_id: OWNER.repositoryId,
      branch: "main",
      root_dir: ".",
      build: { buildMethod: "dockerfile", dockerfilePath: "Dockerfile" },
    }],
    target: [spec("web", web)],
    changed: rule === null ? [] : ["web"],
  })) as RuleView;

/** A kept rule at generation `n`, each approved over the last. */
function kept(n: number) {
  let rule: RuleView["rule"] = null;
  for (let generation = 1; generation <= n; generation += 1) {
    rule = { body: draftRule(view(rule, `v${generation}`), SCOPE, OWNER.ownerId, generation).body, signatures: [] };
  }
  return rule;
}

/** The passkey's answers, in order: `cancel` is a prompt the person dismissed. */
function passkey(...answers: ("cancel" | "answer")[]) {
  const bytes = (text: string) => new TextEncoder().encode(text).buffer;
  const get = vi.fn(async () => answers.shift() === "cancel" ? null : {
    rawId: bytes("credential"),
    response: { authenticatorData: bytes("data"), clientDataJSON: bytes("{}"), signature: bytes("signature") },
  });
  Object.defineProperty(navigator, "credentials", { value: { get }, configurable: true });
  return get;
}

/**
 * The dialog over `first`, as the Settings section shows it. `commit` gets the signed rule and a way to swap the
 * view, as the Store's refusal rereads it.
 */
function open(first: RuleView, commit: (rule: Signed, refresh: (next: RuleView) => void) => Promise<void>) {
  const onClose = vi.fn();
  function Dialog() {
    const [shown, setShown] = useState(first);
    return (
      <RuleDialog organizationSlug="acme" view={shown} ending={false} onClose={onClose}
        scope={{ enforced: true, older: null, scope: SCOPE }}
        commit={(rule) => commit(rule, setShown)} readOwner={async () => OWNER} />
    );
  }
  render(<QueryClientProvider client={new QueryClient()}><Dialog /></QueryClientProvider>);
  return onClose;
}

const approve = async () => {
  const button = await screen.findByRole("button", { name: "Approve with passkey" });
  await waitFor(() => expect(button.hasAttribute("disabled")).toBe(false));
  fireEvent.click(button);
};

const generation = (rule: Signed | undefined) => rule === undefined ? null : JSON.parse(rule.body).generation;

afterEach(() => { cleanup(); });

it("lets a person try again after cancelling the passkey prompt", async () => {
  const get = passkey("cancel", "answer");
  const signed: Signed[] = [];
  const onClose = open(view(null, "v1"), async (rule) => { signed.push(rule); });
  await approve();
  expect((await screen.findByRole("alert")).textContent).toBe("Your passkey didn't answer. Try again.");
  expect(signed).toEqual([]);

  await approve();
  await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
  expect(get).toHaveBeenCalledTimes(2);
  expect(signed.map(generation)).toEqual([1]);
});

it("approves the newer kept rule's next generation after the Store refuses a stale one", async () => {
  passkey("answer", "answer");
  const signed: Signed[] = [];
  const onClose = open(view(kept(1), "v2"), async (rule, refresh) => {
    signed.push(rule);
    if (signed.length > 1) return;
    // Another tab approved generation 2 first: the Store refuses this one, and its refusal rereads the view.
    refresh(view(kept(2), "v3"));
    throw new Error("A newer approval of this rule is already kept. Approve it again.");
  });
  await approve();
  await waitFor(() => expect(signed.map(generation)).toEqual([2]));
  expect(onClose).not.toHaveBeenCalled();

  await approve();
  await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
  expect(signed.map(generation)).toEqual([2, 3]);
});
