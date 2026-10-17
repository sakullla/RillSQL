import { describe, expect, it } from "vitest";
import { createSession } from "./session";

describe("session query actions", () => {
  it("increments the nonce so repeated requests fire", () => {
    const session = createSession("");
    expect(session.pendingQueryAction.value).toBeNull();
    session.requestQueryAction("run");
    expect(session.pendingQueryAction.value).toMatchObject({ action: "run", nonce: 1 });
    session.requestQueryAction("run");
    expect(session.pendingQueryAction.value).toMatchObject({ action: "run", nonce: 2 });
  });
});
