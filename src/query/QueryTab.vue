<script setup lang="ts">
import { sql } from "@codemirror/lang-sql";
import { EditorState } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { basicSetup } from "codemirror";
import { onBeforeUnmount, onMounted, ref } from "vue";

const props = defineProps<{ tabId: string; draft: string }>();
const emit = defineEmits<{ "update:draft": [tabId: string, draft: string] }>();

const host = ref<HTMLElement | null>(null);
let view: EditorView | undefined;
let alive = true;

const shellTheme = EditorView.theme({
  "&": { backgroundColor: "var(--panel)", color: "var(--text)", height: "100%" },
  ".cm-content": { fontFamily: "var(--mono)", fontSize: "var(--editor-font)", caretColor: "var(--text)" },
  ".cm-gutters": { backgroundColor: "var(--panel)", color: "var(--text-3)", borderRight: "1px solid var(--line)" },
  "&.cm-focused": { outline: "none" },
});

onMounted(() => {
  if (!host.value) return;
  view = new EditorView({
    parent: host.value,
    state: EditorState.create({
      doc: props.draft,
      extensions: [
        basicSetup,
        sql(),
        shellTheme,
        EditorView.updateListener.of((update) => {
          if (!alive || !update.docChanged) return;
          const draft = update.state.doc.toString();
          if (draft !== props.draft) emit("update:draft", props.tabId, draft);
        }),
      ],
    }),
  });
});

onBeforeUnmount(() => {
  alive = false;
  view?.destroy();
});
</script>

<template>
  <section class="query-placeholder" aria-label="查询" :data-tab-id="tabId">
    <p class="placeholder-note">执行目标、运行和取消会在查询模块接入。这里只保留可编辑的 SQL 入口。</p>
    <div ref="host" class="cm-host"></div>
  </section>
</template>

<style scoped>
.query-placeholder { display: flex; flex-direction: column; flex: 1; min-height: 0; }
</style>
