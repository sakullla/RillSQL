<script setup lang="ts">
import { computed, nextTick, ref, toRef, watch } from "vue";
import { detectMac, formatChord } from "../shell/shortcuts";
import type { MenuItem } from "./types";
import { useOutsideDismiss } from "./useOutsideDismiss";

const props = withDefaults(
  defineProps<{ open: boolean; x: number; y: number; items: MenuItem[]; label?: string; trigger?: HTMLElement | null }>(),
  { label: "操作", trigger: null },
);

const emit = defineEmits<{ close: []; select: [id: string] }>();

/** 菜单宽度与行高常量：jsdom 量不到布局，退回这些值；真机行高读 --control-h-sm。 */
const MENU_W = 208;
const ITEM_H = 24;
const HEAD_H = 26;
const SEP_H = 9;
const EDGE = 8;

const menu = ref<HTMLElement | null>(null);

const mac = detectMac();

/** 菜单里展示的快捷键统一走 formatChord：调用方继续传 shortcuts.ts 的原始 chord（如 Mod+T）。 */
function displayShortcut(chord: string): string {
  return formatChord(chord, mac);
}

/** 关闭时把焦点还给打开菜单的元素，六个调用方不必各写一遍。 */
let restoreTo: HTMLElement | null = null;

/** 行高取自令牌，jsdom 里 getComputedStyle 读不到自定义属性时退回常量。 */
function rowHeight(): number {
  const raw = getComputedStyle(document.documentElement).getPropertyValue("--control-h-sm");
  const px = Number.parseFloat(raw);
  return Number.isFinite(px) && px > 0 ? px : ITEM_H;
}

function estimatedHeight(): number {
  return props.items.reduce((total, item) => {
    if (item.kind === "item") return total + rowHeight();
    if (item.kind === "head") return total + HEAD_H;
    return total + SEP_H;
  }, 12);
}

const position = computed(() => ({
  left: `${Math.max(EDGE, Math.min(props.x, window.innerWidth - MENU_W - EDGE))}px`,
  top: `${Math.max(EDGE, Math.min(props.y, window.innerHeight - estimatedHeight() - EDGE))}px`,
}));

function focusables(): HTMLElement[] {
  if (!menu.value) return [];
  return [...menu.value.querySelectorAll<HTMLElement>("button.menu-item:not(:disabled)")];
}

function focusItem(index: number) {
  const items = focusables();
  if (!items.length) return;
  const wrapped = ((index % items.length) + items.length) % items.length;
  items[wrapped]?.focus();
}

function onKey(event: KeyboardEvent) {
  const items = focusables();
  const at = items.indexOf(document.activeElement as HTMLElement);
  if (event.key === "Escape") {
    event.preventDefault();
    emit("close");
    return;
  }
  if (event.key === "Tab") {
    emit("close");
    return;
  }
  if (event.key === "ArrowDown") {
    event.preventDefault();
    focusItem(at + 1);
    return;
  }
  if (event.key === "ArrowUp") {
    event.preventDefault();
    focusItem(at < 0 ? items.length - 1 : at - 1);
    return;
  }
  if (event.key === "Home") {
    event.preventDefault();
    focusItem(0);
    return;
  }
  if (event.key === "End") {
    event.preventDefault();
    focusItem(items.length - 1);
  }
}

function pick(item: MenuItem) {
  if (item.kind !== "item" || item.disabled) return;
  emit("select", item.id);
  emit("close");
}

function itemKey(item: MenuItem, index: number): string {
  if (item.kind === "item") return item.id;
  if (item.kind === "head") return `head-${index}`;
  return `sep-${index}`;
}

const openRef = toRef(props, "open");
useOutsideDismiss(
  openRef,
  (target) => Boolean(menu.value?.contains(target) || props.trigger?.contains(target)),
  () => emit("close"),
);

watch(
  openRef,
  (isOpen) => {
    if (isOpen) {
      const active = document.activeElement;
      restoreTo = active instanceof HTMLElement ? active : null;
      void nextTick(() => focusItem(0));
    } else {
      restoreTo?.focus();
      restoreTo = null;
    }
  },
  { immediate: true },
);
</script>

<template>
  <Teleport to="body">
    <div
      v-if="open"
      ref="menu"
      class="menu"
      role="menu"
      :aria-label="label"
      :style="position"
      @click.stop
      @contextmenu.prevent
      @keydown="onKey"
    >
      <template v-for="(item, index) in items" :key="itemKey(item, index)">
        <div v-if="item.kind === 'head'" class="menu-head" role="presentation">{{ item.text }}</div>
        <div v-else-if="item.kind === 'sep'" class="menu-sep" role="separator"></div>
        <button
          v-else
          class="menu-item"
          :class="{ danger: item.danger }"
          type="button"
          role="menuitem"
          :data-id="item.id"
          :disabled="item.disabled"
          :title="item.title"
          :aria-checked="item.checked === undefined ? undefined : item.checked"
          @click="pick(item)"
        >
          <span>{{ item.label }}</span>
          <svg v-if="item.checked" class="i check" viewBox="0 0 24 24" aria-hidden="true"><path d="m5 12.5 4.5 4.5L19 7" /></svg>
          <span v-if="item.shortcut" class="menu-key">{{ displayShortcut(item.shortcut) }}</span>
        </button>
      </template>
    </div>
  </Teleport>
</template>
