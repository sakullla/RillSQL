<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref, watch } from "vue";
import type { MenuItem } from "./types";

const props = withDefaults(
  defineProps<{ open: boolean; x: number; y: number; items: MenuItem[]; label?: string }>(),
  { label: "操作" },
);

const emit = defineEmits<{ close: []; select: [id: string] }>();

/** 菜单宽度与单项高度是常量，位置在渲染前算好：jsdom 下取不到 offsetWidth，测试也要能断言钳制结果。 */
const MENU_W = 208;
const ITEM_H = 24;
const HEAD_H = 26;
const SEP_H = 9;
const EDGE = 8;

const menu = ref<HTMLElement | null>(null);

const estimatedHeight = computed(() =>
  props.items.reduce((total, item) => {
    if (item.kind === "item") return total + ITEM_H;
    if (item.kind === "head") return total + HEAD_H;
    return total + SEP_H;
  }, 12),
);

const position = computed(() => ({
  left: `${Math.max(EDGE, Math.min(props.x, window.innerWidth - MENU_W - EDGE))}px`,
  top: `${Math.max(EDGE, Math.min(props.y, window.innerHeight - estimatedHeight.value - EDGE))}px`,
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

function onOutside(event: PointerEvent) {
  const target = event.target;
  if (target instanceof Node && menu.value?.contains(target)) return;
  emit("close");
}

function onKey(event: KeyboardEvent) {
  const items = focusables();
  const at = items.indexOf(document.activeElement as HTMLElement);
  if (event.key === "Escape") {
    event.preventDefault();
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

watch(
  () => props.open,
  (isOpen) => {
    if (isOpen) {
      document.addEventListener("pointerdown", onOutside, true);
      void nextTick(() => focusItem(0));
    } else {
      document.removeEventListener("pointerdown", onOutside, true);
    }
  },
  { immediate: true },
);

onBeforeUnmount(() => document.removeEventListener("pointerdown", onOutside, true));
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
      <template v-for="(item, index) in items" :key="index">
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
          @click="pick(item)"
        >
          <span>{{ item.label }}</span>
          <span v-if="item.shortcut" class="menu-key">{{ item.shortcut }}</span>
        </button>
      </template>
    </div>
  </Teleport>
</template>
