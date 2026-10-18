<script setup lang="ts">
import { computed, nextTick, onMounted, ref, watch } from "vue";
import ChordKeys from "./ChordKeys.vue";
import { detectMac, SHORTCUTS, shortcutPlatformNote, type MenuName, type ShortcutDef } from "./shortcuts";

const props = defineProps<{ open: boolean }>();
const emit = defineEmits<{ close: [] }>();

const mac = computed(() => detectMac());
// 与 src/shell/menus.ts 菜单栏顺序保持一致
const MENU_ORDER: MenuName[] = ["文件", "编辑", "视图", "查询", "工具", "窗口", "帮助"];
const dialog = ref<HTMLDialogElement | null>(null);

const grouped = computed(() => {
  const sections: { title: string; items: ShortcutDef[] }[] = [];
  for (const menuName of MENU_ORDER) {
    const items = SHORTCUTS.filter((s) => s.menus?.includes(menuName));
    if (items.length) sections.push({ title: menuName, items });
  }
  const editor = SHORTCUTS.filter((s) => s.group === "SQL 编辑器" && !s.menus?.length);
  if (editor.length) sections.push({ title: "SQL 编辑器", items: editor });
  const grid = SHORTCUTS.filter((s) => s.group === "表格" && !s.menus?.length);
  if (grid.length) sections.push({ title: "表格", items: grid });
  const rest = SHORTCUTS.filter((s) => s.group === "全局" && !s.menus?.length);
  if (rest.length) sections.push({ title: "其它", items: rest });
  return sections;
});

async function syncDialog(open: boolean) {
  await nextTick();
  const el = dialog.value;
  if (!el) return;
  if (open) {
    if (!el.open) {
      if (typeof el.showModal === "function") el.showModal();
      else el.setAttribute("open", "");
    }
    return;
  }
  if (el.open) {
    if (typeof el.close === "function") el.close();
    else el.removeAttribute("open");
  }
}

watch(() => props.open, (open) => void syncDialog(open));
onMounted(() => void syncDialog(props.open));
</script>

<template>
  <dialog ref="dialog" class="modal modal-lg" aria-labelledby="shortcut-title" @cancel.prevent="emit('close')" @click.self="emit('close')">
    <div class="modal-head">
      <div class="modal-titles">
        <h2 id="shortcut-title">键盘快捷键</h2>
        <p class="modal-sub">{{ shortcutPlatformNote(mac) }}</p>
      </div>
    </div>
    <div class="modal-body">
      <div class="keys-grid">
        <section v-for="section in grouped" :key="section.title">
          <h4>{{ section.title }}</h4>
          <div v-for="item in section.items" :key="item.chord + item.label" class="key-row">
            <span>{{ item.label }}</span>
            <ChordKeys :chord="item.chord" :mac="mac" />
          </div>
        </section>
      </div>
    </div>
    <div class="modal-foot">
      <span class="grow"></span>
      <button class="btn primary" type="button" @click="emit('close')">知道了</button>
    </div>
  </dialog>
</template>
