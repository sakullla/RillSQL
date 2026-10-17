<script setup lang="ts">
import { computed, ref, watch } from "vue";
import ContextMenu from "../ui/ContextMenu.vue";
import type { MenuDefinition } from "./menu-model";

const props = defineProps<{ menus: MenuDefinition[] }>();
const emit = defineEmits<{ select: [menuId: string, itemId: string] }>();

const buttons = ref<HTMLElement[]>([]);
const openId = ref<string | null>(null);
const anchor = ref<{ x: number; y: number }>({ x: 0, y: 0 });

const active = computed(() => props.menus.find((menu) => menu.id === openId.value) ?? null);
const openIndex = computed(() => props.menus.findIndex((menu) => menu.id === openId.value));

// menus 变化导致当前打开项消失时，ContextMenu 会因 v-if 卸载而不发 close，这里兜底清 openId
watch(active, (value) => {
  if (!value) openId.value = null;
});

function toggle(menu: MenuDefinition, index: number) {
  const button = buttons.value[index];
  if (!button) return;
  if (openId.value === menu.id) {
    openId.value = null;
    return;
  }
  const rect = button.getBoundingClientRect();
  anchor.value = { x: rect.left, y: rect.bottom + 2 };
  openId.value = menu.id;
}

function onBarKey(event: KeyboardEvent, index: number) {
  if (event.key === "ArrowRight") {
    event.preventDefault();
    buttons.value[(index + 1) % props.menus.length]?.focus();
    return;
  }
  if (event.key === "ArrowLeft") {
    event.preventDefault();
    buttons.value[(index - 1 + props.menus.length) % props.menus.length]?.focus();
    return;
  }
  if (event.key === "Escape") {
    openId.value = null;
  }
}

function onItemClick(itemId: string) {
  if (!openId.value) return;
  const menuId = openId.value;
  openId.value = null;
  emit("select", menuId, itemId);
}

function setButtonRef(el: Element | ComponentPublicInstance | null, index: number) {
  if (el instanceof HTMLElement) buttons.value[index] = el;
  else delete buttons.value[index];
}
</script>

<template>
  <div class="menubar" role="menubar" aria-label="主菜单">
    <button
      v-for="(menu, index) in menus"
      :key="menu.id"
      :ref="(el: Element | ComponentPublicInstance | null) => setButtonRef(el, index)"
      type="button"
      class="menubar-item"
      aria-haspopup="menu"
      :aria-expanded="openId === menu.id"
      @click="toggle(menu, index)"
      @keydown="onBarKey($event, index)"
    >
      {{ menu.label }}
    </button>
  </div>
  <ContextMenu
    v-if="active"
    :open="openId !== null"
    :x="anchor.x"
    :y="anchor.y"
    :items="active.items"
    :label="`${active.label} 菜单`"
    :trigger="openIndex >= 0 ? buttons[openIndex] ?? null : null"
    @close="openId = null"
    @select="(itemId) => onItemClick(itemId)"
  />
</template>

<style scoped>
.menubar { display: flex; align-items: center; gap: 2px; }
.menubar-item {
  height: var(--control-h-sm);
  padding: 0 var(--sp-2);
  border-radius: var(--r);
  color: var(--text-2);
  font-size: var(--fs-sm);
  white-space: nowrap;
  transition: background var(--dur-fast) var(--ease), color var(--dur-fast) var(--ease);
}
.menubar-item:hover { background: var(--hover); color: var(--text); }
.menubar-item:active { background: var(--active); }
.menubar-item:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
.menubar-item[aria-expanded="true"] { background: var(--sel); color: var(--text); font-weight: 600; }
</style>
