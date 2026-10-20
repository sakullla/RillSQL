import { EditorView } from "@codemirror/view";

function usable(current: EditorView | null | undefined): current is EditorView {
  if (!current) return false;
  /* destroyed 是私有字段，运行时确实存在；经 unknown 才能按形状读取。 */
  return !(current as unknown as { destroyed?: boolean }).destroyed;
}

/** 热更新会重跑 setup，但页面上的编辑器还在。优先沿用它，丢了再新建。 */
export function resolveEditor(
  current: EditorView | undefined,
  host: HTMLElement | null,
  create: (parent: HTMLElement) => EditorView,
): EditorView | undefined {
  if (usable(current)) return current;
  if (!host) return undefined;
  const existing = host.querySelector(".cm-editor");
  if (existing instanceof HTMLElement) {
    const found = EditorView.findFromDOM(existing);
    if (usable(found)) return found;
    existing.remove();
  }
  return create(host);
}
