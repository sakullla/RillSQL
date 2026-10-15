import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch, type Ref } from "vue";

export function rowWindow(count: number, scrollTop: number, height: number, rowHeight: number, overscan = 6) {
  const size = Math.max(1, rowHeight);
  const start = Math.max(0, Math.min(Math.max(0, count - 1), Math.floor(Math.max(0, scrollTop - size) / size) - overscan));
  const end = Math.min(count, start + Math.ceil(height / size) + overscan * 2 + 1);
  return { start, end, top: start * size, bottom: Math.max(0, (count - end) * size) };
}

/** Fixed-height data rows; observe viewport and density without a listener per row. */
export function useRowWindow(count: Ref<number>, resetOnCount = true) {
  const viewport = ref<HTMLElement | null>(null);
  const top = ref(0);
  const height = ref(400);
  const rowHeight = ref(32);
  let resize: ResizeObserver | undefined;
  let density: MutationObserver | undefined;
  let frame = 0;
  function measure() {
    if (!viewport.value) return;
    height.value = viewport.value.clientHeight || 400;
    rowHeight.value = parseFloat(getComputedStyle(viewport.value).getPropertyValue("--row-h")) || 32;
    top.value = viewport.value.scrollTop;
  }
  function onScroll() {
    if (frame) return;
    frame = requestAnimationFrame(() => { frame = 0; measure(); });
  }
  function reset() { if (viewport.value) viewport.value.scrollTop = 0; top.value = 0; }
  onMounted(() => {
    measure();
    if (typeof ResizeObserver !== "undefined") {
      resize = new ResizeObserver(measure);
      if (viewport.value) resize.observe(viewport.value);
    }
    density = new MutationObserver(measure);
    density.observe(document.documentElement, { attributes: true, attributeFilter: ["data-density"] });
  });
  watch(count, () => { if (resetOnCount) reset(); else void nextTick(measure); });
  function reveal(index: number) {
    const element = viewport.value;
    if (!element) return;
    const y = (index + 1) * rowHeight.value;
    if (y < element.scrollTop + rowHeight.value) element.scrollTop = Math.max(0, y - rowHeight.value);
    else if (y + rowHeight.value > element.scrollTop + height.value) element.scrollTop = y + rowHeight.value - height.value;
    measure();
  }
  onBeforeUnmount(() => { resize?.disconnect(); density?.disconnect(); cancelAnimationFrame(frame); });
  const range = computed(() => rowWindow(count.value, top.value, height.value, rowHeight.value));
  return { viewport, range, onScroll, reset, reveal };
}
