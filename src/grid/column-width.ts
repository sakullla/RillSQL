/** 网格列宽工具：拖拽与双击自适应共用的夹取与测宽。 */

export const MIN_COL_WIDTH = 48;
export const MAX_COL_WIDTH = 360;

/** 把任意像素值夹到 [48, 360]；非有限值回落到最小宽度。 */
export function clampColWidth(px: number): number {
  const rounded = Math.round(px);
  if (!Number.isFinite(rounded)) return MIN_COL_WIDTH;
  return Math.min(MAX_COL_WIDTH, Math.max(MIN_COL_WIDTH, rounded));
}

/** 按最长样本估算内容宽度；CJK 按 2 个字宽计，结果走 clampColWidth。 */
export function autofitWidth(samples: string[], charPx = 7.2, padPx = 22): number {
  let widest = 0;
  for (const sample of samples) {
    let units = 0;
    for (const char of sample) {
      units += (char.codePointAt(0) ?? 0) > 0x2e80 ? 2 : 1;
    }
    if (units > widest) widest = units;
  }
  return clampColWidth(widest * charPx + padPx);
}
