import { describe, expect, it } from "vitest";
import { setLocale } from "./index";
import { backendErrorText, codeHint, codeMessage, errorCodeOf, parseCodedText } from "./backend-error";

/** 后端错误码桥接（T6-A2/R6，T6-A3 修订）：非 zh-CN 按码取字典；zh-CN 原文直出（保留动态细节）；无码/未知码回退原文。 */

const CODED = "[import.passphrase_required] 这份文件带口令加密，请输入导出时设置的口令。";
const NETWORK = "[conn.network_failed] connection refused (os error 111) during TLS handshake with db.internal:5432";
const SESSION_GAP = '[dump.session_gap current="bookstore" database="reports"] 当前连接打开的是 bookstore，没有另开连接读取 reports';

describe("backend error code bridge", () => {
  it("parses the [code] prefix and keeps the original text", () => {
    const parsed = parseCodedText(CODED);
    expect(parsed.code).toBe("import.passphrase_required");
    expect(parsed.params).toEqual({});
    expect(parsed.text).toBe("这份文件带口令加密，请输入导出时设置的口令。");
    const plain = parseCodedText("普通错误");
    expect(plain.code).toBeNull();
    expect(plain.text).toBe("普通错误");
  });

  it("parses interpolation params carried in the coded prefix (T6-A3)", () => {
    const parsed = parseCodedText(SESSION_GAP);
    expect(parsed.code).toBe("dump.session_gap");
    expect(parsed.params).toEqual({ current: "bookstore", database: "reports" });
    expect(parsed.text).toBe("当前连接打开的是 bookstore，没有另开连接读取 reports");
  });

  it("extracts codes from strings, Errors and coded objects (errorCodeOf)", () => {
    expect(errorCodeOf(CODED)).toBe("import.passphrase_required");
    expect(errorCodeOf(new Error(SESSION_GAP))).toBe("dump.session_gap");
    expect(errorCodeOf({ code: "conn.password_required", message: "x" })).toBe("conn.password_required");
    expect(errorCodeOf({ message: CODED })).toBe("import.passphrase_required");
    expect(errorCodeOf("没有任何码的错误")).toBeNull();
    expect(errorCodeOf(null)).toBeNull();
  });

  it("keeps the zh-CN driver text verbatim (no dictionary override)", () => {
    expect(backendErrorText(NETWORK)).toBe("connection refused (os error 111) during TLS handshake with db.internal:5432");
    expect(codeMessage("conn.network_failed")).toBeNull();
    expect(codeMessage("conn.password_required")).toBeNull();
    expect(codeHint("conn.password_required")).toBeNull();
    expect(backendErrorText(SESSION_GAP)).toBe("当前连接打开的是 bookstore，没有另开连接读取 reports");
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

  it("shows the dictionary summary in English while zh keeps driver detail (T6-A3)", () => {
    setLocale("en");
    try {
      expect(backendErrorText(NETWORK)).toBe("Network connection failed");
      expect(codeMessage("conn.network_failed")).toBe("Network connection failed");
    } finally {
      setLocale("zh-CN");
    }
    // zh 回退驱动原文，细节（地址/错误码）不丢。
    expect(backendErrorText(NETWORK)).toContain("db.internal:5432");
  });

  it("interpolates database names for dump.session_gap in English (T6-A3)", () => {
    setLocale("en");
    try {
      const text = backendErrorText(SESSION_GAP);
      expect(text).toContain("bookstore");
      expect(text).toContain("reports");
    } finally {
      setLocale("zh-CN");
    }
  });

  it("resolves structured code fields for reports (message + hint) outside zh-CN", () => {
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
    setLocale("en");
    try {
      expect(codeMessage("not-a-code")).toBeNull();
      expect(codeHint("import.passphrase_required")).toBeNull(); // 该码无 hint
    } finally {
      setLocale("zh-CN");
    }
  });
});
