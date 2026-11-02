import { tr } from "../i18n";

export const PRODUCT_VERSION = "0.2.3";
export const PRODUCT_LICENSE = "GPL-3.0-only";

/** 窗口/文档标题：品牌名不随界面语言变化（品牌专名，两语言一致）。 */
export function productTitle(): string {
  return tr("shell.product.title");
}

export function aboutTagline(): string {
  return tr("shell.about.tagline");
}

export function aboutAbsent(): string {
  return tr("shell.about.absent");
}

export function aboutFacts(): string[] {
  return [tr("shell.about.factOffline"), tr("shell.about.factNoCloud"), tr("shell.about.factNoAi")];
}
