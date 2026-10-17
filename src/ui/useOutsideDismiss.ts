import { onScopeDispose, watch, type Ref } from "vue";

/**
 * 浮层外点关闭：在捕获阶段监听 document 的 pointerdown，
 * 目标不在 isInside 范围内时调用 onDismiss。
 * 所有 src/ui 浮层（Listbox / ContextMenu / 菜单栏下拉）共用这一份实现。
 */
export function useOutsideDismiss(
  open: Ref<boolean>,
  isInside: (target: Node) => boolean,
  onDismiss: () => void,
): void {
  function onPointerDown(event: PointerEvent) {
    const target = event.target;
    if (!(target instanceof Node)) return;
    if (isInside(target)) return;
    onDismiss();
  }

  // 默认 pre-flush：真实指针事件是宏任务，Vue 的微任务刷新总在其之前，
  // 所以 open 之后的下一次点击必然已被监听；不加 flush: "sync"。
  watch(
    open,
    (isOpen) => {
      if (isOpen) document.addEventListener("pointerdown", onPointerDown, true);
      else document.removeEventListener("pointerdown", onPointerDown, true);
    },
    { immediate: true },
  );

  // 组件卸载时 instance.scope.stop() 会同步跑这里；effectScope.stop() 同理。
  onScopeDispose(() => document.removeEventListener("pointerdown", onPointerDown, true));
}
