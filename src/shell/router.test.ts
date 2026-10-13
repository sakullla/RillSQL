import { describe, expect, it } from "vitest";
import { parseHash, toHash } from "../router";

describe("router", () => {
  it("opens the workspace welcome route and ignores review parameters", () => {
    expect(parseHash("")).toBe("workspace");
    expect(parseHash("#/")).toBe("workspace");
    expect(parseHash("#/settings")).toBe("settings");
    expect(parseHash("#/connections?scenario=tls")).toBe("connections");
    expect(parseHash("#/login")).toBe("workspace");
    expect(parseHash("#/?demo=about")).toBe("workspace");
    expect(toHash("history")).toBe("#/history");
    expect(toHash("workspace")).toBe("#/");
  });
});
