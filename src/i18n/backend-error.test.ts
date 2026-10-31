import { describe, expect, it } from "vitest";
import { setLocale } from "./index";
import { backendErrorText, codeHint, codeMessage, parseCodedText } from "./backend-error";

/** 后端错误码桥接（T6-A2/R6）：码→字典文案；无码/未知码回退原文；中英两包齐备。 */

const CODED = "[import.passphrase_required] 这份文件带口令加密，请输入导出时设置的口令。";

describe("backend error code bridge", () => {
  it("parses the [code] prefix and keeps the original text", () => {
    const parsed = parseCodedText(CODED);
    expect(parsed.code).toBe("import.passphrase_required");
    expect(parsed.text).toBe("这份文件带口令加密，请输入导出时设置的口令。");
    const plain = parseCodedText("普通错误");
    expect(plain.code).toBeNull();
    expect(plain.text).toBe("普通错误");
  });

  it("translates a coded string into the zh dictionary entry", () => {
    expect(backendErrorText(CODED)).toBe("这份文件带口令加密，请输入导出时设置的口令。");
  });

  it("translates the same code in English after switching locale", () => {
    setLocale("en");
    try {
      expect(backendErrorText(CODED)).toContain("passphrase-encrypted");
    } finally {
      setLocale("zh-CN");
    }
  });

  it("resolves structured code fields for reports (message + hint)", () => {
    expect(codeMessage("conn.password_required")).toBe("请填写数据库密码");
    expect(codeHint("conn.password_required")).toContain("凭据库没有可用密码");
    setLocale("en");
    try {
      expect(codeMessage("conn.password_required")).toBe("Enter the database password");
      expect(codeHint("conn.auth_failed")).toContain("username and password");
    } finally {
      setLocale("zh-CN");
    }
  });

  it("bridges Error instances carrying the coded prefix", () => {
    expect(backendErrorText(new Error(CODED))).toBe("这份文件带口令加密，请输入导出时设置的口令。");
  });

  it("falls back to the original text for unknown or missing codes", () => {
    expect(backendErrorText("[nonsense.unknown_code] 原始错误")).toBe("原始错误");
    expect(backendErrorText("没有任何码的错误")).toBe("没有任何码的错误");
    expect(backendErrorText(new Error("纯 Error 文本"))).toBe("纯 Error 文本");
    expect(backendErrorText({ message: "对象里的原文" })).toBe("对象里的原文");
    expect(backendErrorText(null, "兜底文案")).toBe("兜底文案");
    expect(backendErrorText(undefined, "兜底文案")).toBe("兜底文案");
  });

  it("returns null for empty/invalid codes in code helpers", () => {
    expect(codeMessage(null)).toBeNull();
    expect(codeMessage("")).toBeNull();
    expect(codeMessage("not-a-code")).toBeNull();
    expect(codeHint("import.passphrase_required")).toBeNull(); // 该码无 hint
  });
});
