import { createI18n } from "vue-i18n";
import { loadSettings, normalizeLocale, type AppLocale } from "../shell/settings";
import { zhCN } from "./zh-CN";
import { en } from "./en";

/** zh-CN 字典结构：en 字典按同一结构做类型约束，缺 key 在 typecheck 阶段报错（编译期语言包完整性检查）。 */
export type MessageSchema = typeof zhCN;

/** strict key 提示：从 zh-CN 字典结构推导全部可用 key（可选约束，typo 时 typecheck 报错）。 */
type NestedKeyOf<T> = T extends string
  ? never
  : { [K in keyof T & string]: T[K] extends string ? K : `${K}.${NestedKeyOf<T[K]>}` }[keyof T & string];
export type MessageKey = NestedKeyOf<MessageSchema>;

export const LOCALES: readonly AppLocale[] = ["zh-CN", "en"];

export const i18n = createI18n({
  legacy: false,
  locale: normalizeLocale(loadSettings().locale),
  fallbackLocale: "zh-CN",
  messages: { "zh-CN": zhCN, en },
  missingWarn: false,
  fallbackWarn: false,
});

/**
 * 翻译入口：组件模板、计算属性与非组件 TS 模块（shortcuts/commands/status-text 等）统一走这里。
 * locale 是 vue-i18n 的响应式 ref，渲染期读取建立依赖，切换语言即时重渲染（R6 无需重载）。
 */
export function tr(key: MessageKey | (string & {}), params?: Record<string, unknown>): string {
  void i18n.global.locale.value;
  return i18n.global.t(key, params ?? {});
}

/** 组件外的组合入口：const t = useT() 与 tr 等价，便于与习惯的 t("key") 写法对齐。 */
export function useT(): typeof tr {
  return tr;
}

export function currentLocale(): AppLocale {
  return normalizeLocale(i18n.global.locale.value);
}

/** 切换语言（R6）：响应式生效，调用方负责持久化到 ShellSettings。 */
export function setLocale(locale: AppLocale): void {
  i18n.global.locale.value = locale;
  if (typeof document !== "undefined") document.documentElement.lang = locale;
}

if (typeof document !== "undefined") {
  document.documentElement.lang = normalizeLocale(loadSettings().locale);
}
