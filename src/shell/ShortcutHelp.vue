<script setup lang="ts">
import { computed, nextTick, onMounted, ref, watch } from "vue";
import { tr as t } from "../i18n";
import ChordKeys from "./ChordKeys.vue";
import { detectMac, SHORTCUTS, shortcutPlatformNote, type ShortcutGroup } from "./shortcuts";

const props = defineProps<{ open: boolean }>();
const emit = defineEmits<{ close: [] }>();

const mac = computed(() => detectMac());
const groups: { id: ShortcutGroup; label: string }[] = [
  { id: "global", label: "shell.shortcuts.groupGlobal" },
  { id: "editor", label: "shell.shortcuts.groupEditor" },
  { id: "table", label: "shell.shortcuts.groupGrid" },
];
const dialog = ref<HTMLDialogElement | null>(null);

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
        <h2 id="shortcut-title">{{ t("shell.shortcuts.title") }}</h2>
        <p class="modal-sub">{{ shortcutPlatformNote(mac) }}</p>
      </div>
    </div>
    <div class="modal-body">
      <div class="keys-grid">
        <section v-for="group in groups" :key="group.id">
          <h4>{{ t(group.label) }}</h4>
          <div v-for="item in SHORTCUTS.filter((entry) => entry.group === group.id)" :key="item.chord + item.label" class="key-row">
            <span>{{ t(item.label) }}</span>
            <ChordKeys :chord="item.chord" :mac="mac" />
          </div>
        </section>
      </div>
    </div>
    <div class="modal-foot">
      <span class="grow"></span>
      <button class="btn primary" type="button" @click="emit('close')">{{ t("shell.shortcuts.gotIt") }}</button>
    </div>
  </dialog>
</template>
