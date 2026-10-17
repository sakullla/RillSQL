import { describe, expect, it } from "vitest";
import { applyDocumentTheme, resolvedTheme } from "./theme";

describe("theme", () => {
  it("resolves system theme from the color scheme instead of a third palette", () => {
    expect(resolvedTheme("system", true)).toBe("dark");
    expect(resolvedTheme("system", false)).toBe("light");
    expect(resolvedTheme("dark", false)).toBe("dark");
    expect(resolvedTheme("light", true)).toBe("light");
  });

  it("writes the resolved theme and density onto the document", () => {
    const root = document.createElement("html");
    applyDocumentTheme("system", "standard", true, 15, root);
    expect(root.dataset.theme).toBe("dark");
    expect(root.dataset.density).toBe("standard");
    expect(root.style.getPropertyValue("--editor-font")).toBe("15px");
  });
});
