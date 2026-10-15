// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import { ServiceCommandField } from "./ServiceCommandField";

afterEach(cleanup);

it("shows the committed command while the write runs, and the stored one again when it fails", async () => {
  let fail = (_error: Error) => {};
  const write = { promise: new Promise<void>((_resolve, reject) => { fail = reject; }) };
  const field = (value: string | null) => (
    <ServiceCommandField label="Start command" placeholder="npm start" value={value} isChanged={false} validate={() => null}
      onCommit={() => ({ isPersisted: { promise: write.promise } })} />
  );
  const { rerender } = render(field("node server.js"));
  const input = screen.getByLabelText<HTMLInputElement>("Start command");
  fireEvent.change(input, { target: { value: "node main.js" } });
  fireEvent.keyDown(input, { key: "Enter" });
  rerender(field("node server.js"));
  expect(input.value).toBe("node main.js");

  await act(async () => {
    fail(new Error("refused"));
    await write.promise.catch(() => {});
  });
  expect(input.value).toBe("node server.js");
});
