// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { InviteDialog } from "./members-settings";

describe("InviteDialog", () => {
  afterEach(() => {
    cleanup();
    document.body.replaceChildren();
  });

  it.each([
    { mail: true, emailed_to: "grace@example.com", message: "Command says: emailed to grace@example.com." },
    { mail: false, emailed_to: null, message: "Command says: send the link yourself." },
  ])("shows the command's own message after inviting (mail $mail)", async ({ mail, emailed_to, message }) => {
    const invite = vi.fn().mockResolvedValue({
      id: "inv-1",
      email: "grace@example.com",
      role: "member",
      link: "https://cloud.test/invite/t",
      expires_at: "2026-10-21T00:00:00Z",
      emailed_to,
      message: `${message}\nhttps://cloud.test/invite/t`,
    });
    render(
      <QueryClientProvider client={new QueryClient()}>
        <InviteDialog organizationName="Acme" mail={mail} invite={invite} onInvited={vi.fn()} onOpenChange={vi.fn()} />
      </QueryClientProvider>,
    );

    fireEvent.change(screen.getByLabelText("Email (optional)"), { target: { value: "grace@example.com" } });
    fireEvent.click(screen.getByRole("button", { name: "Make link" }));

    expect((await screen.findByLabelText<HTMLInputElement>("Invite link")).value).toBe("https://cloud.test/invite/t");
    expect(screen.getByText(message)).toBeTruthy();
    expect(invite).toHaveBeenCalledWith({ email: "grace@example.com", role: "member" });
  });
});
