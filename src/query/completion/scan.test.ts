import { describe, expect, it } from "vitest";
import { locateForCompletion } from "./scan";

describe("completion statement scan", () => {
  it("finds the current statement after the last semicolon", () => {
    const text = "SELECT 1;\nSELECT * FROM orders ";
    const found = locateForCompletion(text, text.length);
    expect(text.slice(found.start, found.end).trimStart()).toBe("SELECT * FROM orders ");
    expect(found.refusal).toBe(false);
  });

  it("ignores semicolons inside strings and dollar quotes", () => {
    const text = "SELECT ';' AS semi, $$ ; $$ AS body FROM t ";
    const found = locateForCompletion(text, text.length);
    expect(found.start).toBe(0);
    expect(found.refusal).toBe(false);
  });

  it("reports refusal when a string or comment stays open", () => {
    const quote = "SELECT * FROM t WHERE name = 'abc";
    expect(locateForCompletion(quote, quote.length).refusal).toBe(true);
    const comment = "SELECT 1 /* open";
    expect(locateForCompletion(comment, comment.length).refusal).toBe(true);
  });
});
