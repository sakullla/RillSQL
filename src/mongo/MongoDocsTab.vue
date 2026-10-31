<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import DocJsonView from "../values/DocJsonView.vue";
import { failureOf, tauriMongoApi, type MongoApi, type MongoDoc } from "./api";
import { DOC_PAGE_SIZE, canPrev, cellText, columnsOf, filterApplied, nextSkip, prettyJson, quickValidateJson } from "./docs";

const props = defineProps<{
  tabId: string;
  connectionId: string;
  connectionName: string;
  database: string;
  collection: string;
  status: string;
  api?: MongoApi;
}>();

const api = computed(() => props.api ?? tauriMongoApi);

const documents = ref<MongoDoc[]>([]);
const total = ref(0);
const skip = ref(0);
const loading = ref(false);
const listError = ref("");

const filterInput = ref("");
const appliedFilter = ref("");
const filterError = ref("");

const note = ref("");
const actionError = ref("");

const viewDoc = ref<MongoDoc | null>(null);

const editorOpen = ref(false);
const editorMode = ref<"edit" | "insert">("edit");
const editorText = ref("");
const editorError = ref("");

const confirmDelete = ref<MongoDoc | null>(null);

const columns = computed(() => columnsOf(documents.value));
const pageEnd = computed(() => skip.value + documents.value.length);
const pageCount = computed(() => (filterApplied(appliedFilter.value) ? `匹配 ${total.value}` : `约 ${total.value} 篇`));

async function loadPage() {
  if (loading.value) return;
  loading.value = true;
  listError.value = "";
  try {
    const page = await api.value.findDocuments(
      props.connectionId,
      props.database,
      props.collection,
      skip.value,
      DOC_PAGE_SIZE,
      appliedFilter.value || null,
    );
    documents.value = page.documents;
    total.value = page.total;
  } catch (error) {
    listError.value = failureOf(error);
  } finally {
    loading.value = false;
  }
}

/** 应用过滤：过滤语法错误统一由服务端 bson 反序列化判定，错误消息自带行列定位（R12），
 *  不做本地拦截——本地 JSON.parse 的报错形态不带行列，反而丢失定位信息。 */
async function applyFilter() {
  const text = filterInput.value.trim();
  appliedFilter.value = text;
  filterError.value = "";
  skip.value = 0;
  await loadPage();
  if (listError.value) filterError.value = listError.value;
}

async function clearFilter() {
  filterInput.value = "";
  await applyFilter();
}

async function turnPage(direction: 1 | -1) {
  const next = nextSkip(skip.value, DOC_PAGE_SIZE, direction);
  if (next === skip.value) return;
  skip.value = next;
  await loadPage();
}

async function refresh() {
  await loadPage();
}

function openView(doc: MongoDoc) {
  viewDoc.value = doc;
}

async function copyJson(doc: MongoDoc) {
  note.value = "";
  actionError.value = "";
  try {
    const text = prettyJson(doc.json);
    await navigator.clipboard.writeText(text);
    note.value = "已复制文档 JSON。";
  } catch {
    actionError.value = "复制失败：浏览器剪贴板不可用。";
  }
}

function openEdit(doc: MongoDoc) {
  editorMode.value = "edit";
  editorText.value = prettyJson(doc.json);
  editorError.value = "";
  editorOpen.value = true;
  viewDoc.value = doc;
}

function openInsert() {
  editorMode.value = "insert";
  editorText.value = "{\n  \"status\": \"active\"\n}";
  editorError.value = "";
  editorOpen.value = true;
}

async function submitEditor() {
  const text = editorText.value.trim();
  const quick = quickValidateJson(text);
  if (quick) {
    editorError.value = quick;
    return;
  }
  editorError.value = "";
  try {
    if (editorMode.value === "edit" && viewDoc.value) {
      const outcome = await api.value.replaceDocument(props.connectionId, props.database, props.collection, viewDoc.value.idJson, text);
      if (outcome.matched === 0) {
        editorError.value = "没有匹配到这篇文档（可能已被删除），本次没有改动任何数据。";
        return;
      }
      note.value = `已整体覆盖保存（matched ${outcome.matched}）。`;
    } else {
      const outcome = await api.value.insertDocument(props.connectionId, props.database, props.collection, text);
      note.value = `已新增文档，_id = ${outcome.insertedIdJson}。`;
      skip.value = 0;
    }
    editorOpen.value = false;
    await loadPage();
  } catch (error) {
    // 服务端 bson 校验失败：错误消息自带定位/原因。
    editorError.value = failureOf(error);
  }
}

function askDelete(doc: MongoDoc) {
  confirmDelete.value = doc;
}

function cancelDelete() {
  confirmDelete.value = null;
}

async function runDelete() {
  const doc = confirmDelete.value;
  confirmDelete.value = null;
  if (!doc) return;
  try {
    const outcome = await api.value.deleteDocument(props.connectionId, props.database, props.collection, doc.idJson, true);
    note.value = `已删除 ${outcome.deleted} 篇文档。`;
    if (skip.value > 0 && documents.value.length === 1) skip.value = nextSkip(skip.value, DOC_PAGE_SIZE, -1);
    await loadPage();
  } catch (error) {
    actionError.value = failureOf(error);
  }
}

onMounted(loadPage);
</script>

<!-- MongoDB 集合文档工作区（R12）：顶部过滤与翻页 · 中部文档网格（_id + 顶层字段为列）· 行操作查看/复制/编辑/删除 · JSON 编辑带校验反馈。 -->
<template>
  <div class="mongo-tab" :data-connection="connectionId" :data-database="database" :data-collection="collection">
    <header class="md-head">
      <h3 class="mono" data-mongo-collection-title>{{ connectionName }} / {{ database }} / {{ collection }}</h3>
      <form class="md-filter" @submit.prevent="applyFilter">
        <input
          v-model="filterInput"
          class="mono"
          data-field="mongo-filter"
          placeholder='JSON 过滤，如 {"status": "active"}'
          aria-label="JSON 过滤"
        />
        <button class="btn sm" type="submit" data-action="mongo-apply-filter" :disabled="loading">过滤</button>
        <button v-if="filterApplied(appliedFilter)" class="btn sm ghost" type="button" data-action="mongo-clear-filter" :disabled="loading" @click="clearFilter">清除</button>
      </form>
      <p v-if="filterError" class="md-error" role="alert" data-mongo-filter-error>{{ filterError }}</p>
      <div class="md-bar">
        <span class="md-count" data-mongo-count>{{ pageCount }} · 第 {{ skip + 1 }}–{{ pageEnd }} 篇</span>
        <span class="grow"></span>
        <button class="btn sm ghost" type="button" data-action="mongo-refresh" :disabled="loading" @click="refresh">刷新</button>
        <button class="btn sm" type="button" data-action="mongo-prev" :disabled="loading || !canPrev(skip)" @click="turnPage(-1)">上一页</button>
        <button class="btn sm" type="button" data-action="mongo-next" :disabled="loading || pageEnd >= total" @click="turnPage(1)">下一页</button>
        <button class="btn sm primary" type="button" data-action="mongo-insert" @click="openInsert">新增文档</button>
      </div>
      <p v-if="listError" class="md-error" role="alert" data-mongo-list-error>{{ listError }}</p>
      <p v-if="note" class="md-note ok" data-mongo-note>{{ note }}</p>
      <p v-if="actionError" class="md-note err" data-mongo-action-error>{{ actionError }}</p>
    </header>

    <div class="md-grid-wrap">
      <table v-if="documents.length" class="md-grid" aria-label="集合文档">
        <thead>
          <tr>
            <th v-for="column in columns" :key="column" class="mono" :data-column="column">{{ column }}</th>
            <th class="md-ops-col">操作</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="doc in documents" :key="doc.idJson" :data-doc-id="doc.idJson">
            <td v-for="column in columns" :key="column" class="mono md-cell" :data-cell="column" :title="cellText(doc.json, column)">{{ cellText(doc.json, column) }}</td>
            <td class="md-ops">
              <button class="btn xs" type="button" data-action="mongo-view" @click="openView(doc)">查看</button>
              <button class="btn xs" type="button" data-action="mongo-copy" @click="copyJson(doc)">复制</button>
              <button class="btn xs" type="button" data-action="mongo-edit" @click="openEdit(doc)">编辑</button>
              <button class="btn xs danger" type="button" data-action="mongo-delete" @click="askDelete(doc)">删除</button>
            </td>
          </tr>
        </tbody>
      </table>
      <p v-else-if="!loading && !listError" class="md-empty">
        {{ filterApplied(appliedFilter) ? "没有匹配过滤条件的文档。" : "这个集合没有文档。" }}
      </p>
      <p v-else-if="loading" class="md-empty">读取中…</p>
    </div>

    <aside v-if="viewDoc" class="md-view" aria-label="文档 JSON">
      <header class="md-view-head">
        <h4>文档 JSON</h4>
        <span class="md-id mono">{{ viewDoc.idJson }}</span>
        <span class="grow"></span>
        <button class="btn sm ghost" type="button" data-action="mongo-close-view" @click="viewDoc = null">关闭</button>
      </header>
      <DocJsonView :json="viewDoc.json" />
    </aside>

    <div v-if="editorOpen" class="md-overlay" data-menu="editor" role="dialog" aria-label="编辑文档 JSON" @click.self="editorOpen = false">
      <form class="md-card" @submit.prevent="submitEditor">
        <h4>{{ editorMode === "edit" ? "整体覆盖保存文档" : "新增文档" }}</h4>
        <p class="md-hint">提交整份 JSON 覆盖原文档；非法 JSON 会被拦截，不会发出任何写入。</p>
        <textarea v-model="editorText" class="mono" data-field="mongo-editor" rows="14" aria-label="文档 JSON"></textarea>
        <p v-if="editorError" class="md-error" role="alert" data-mongo-editor-error>{{ editorError }}</p>
        <div class="md-card-actions">
          <button class="btn sm" type="button" data-action="mongo-editor-cancel" @click="editorOpen = false">取消</button>
          <button class="btn sm primary" type="submit" data-action="mongo-editor-save">{{ editorMode === "edit" ? "校验并覆盖保存" : "校验并新增" }}</button>
        </div>
      </form>
    </div>

    <div v-if="confirmDelete" class="md-overlay" data-menu="confirm" role="dialog" aria-label="删除文档确认" @click.self="cancelDelete">
      <div class="md-card">
        <h4>删除文档</h4>
        <p class="md-prompt" data-mongo-confirm-prompt>将删除这篇文档（_id {{ confirmDelete.idJson }}），删除后不可恢复。确认删除？</p>
        <div class="md-card-actions">
          <button class="btn sm" type="button" data-action="mongo-confirm-cancel" @click="cancelDelete">取消</button>
          <button class="btn sm danger" type="button" data-action="mongo-confirm-ok" @click="runDelete">确认删除</button>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.mongo-tab { display: grid; grid-template-columns: 1fr minmax(280px, 34%); grid-template-rows: auto 1fr; gap: var(--sp-2); height: 100%; min-height: 0; padding: var(--sp-2); }
.md-head { grid-column: 1 / -1; display: grid; gap: var(--sp-1); }
.md-head h3 { margin: 0; font-size: 14px; overflow-wrap: anywhere; }
.md-filter { display: flex; gap: var(--sp-1); }
.md-filter input { flex: 1; min-width: 0; }
.md-bar { display: flex; align-items: center; gap: var(--sp-1); }
.md-count { font-size: 12px; color: var(--text-3); }
.md-error { color: var(--danger, #c0392b); font-size: 12px; margin: 0; }
.md-note { font-size: 12px; margin: 0; }
.md-note.ok { color: var(--ok, #2e7d32); }
.md-note.err { color: var(--danger, #c0392b); }
.md-grid-wrap { min-height: 0; overflow: auto; border: 1px solid var(--line); border-radius: var(--r-md, 8px); }
.md-grid { border-collapse: collapse; width: 100%; font-size: 12px; }
.md-grid th { text-align: left; font-size: 11px; color: var(--text-3); padding: var(--sp-1) var(--sp-2); border-bottom: 1px solid var(--line); position: sticky; top: 0; background: var(--bg-1, var(--bg)); white-space: nowrap; }
.md-grid td { padding: var(--sp-1) var(--sp-2); border-bottom: 1px solid var(--line); vertical-align: top; max-width: 320px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.md-ops-col { width: 1%; }
.md-ops { white-space: nowrap; display: flex; gap: var(--sp-1); }
.md-empty { color: var(--text-3); font-size: 12px; padding: var(--sp-3); }
.md-view { min-height: 0; overflow: hidden; display: flex; flex-direction: column; gap: var(--sp-2); border: 1px solid var(--line); border-radius: var(--r-md, 8px); padding: var(--sp-2); }
.md-view-head { display: flex; align-items: center; gap: var(--sp-2); }
.md-view-head h4 { margin: 0; font-size: 13px; }
.md-id { font-size: 11px; color: var(--text-3); overflow-wrap: anywhere; }
.md-view > :not(header) { flex: 1; min-height: 0; }
.md-overlay { position: fixed; inset: 0; background: rgba(0, 0, 0, 0.35); display: grid; place-items: center; z-index: 60; }
.md-card { background: var(--bg, #fff); color: var(--text, inherit); border-radius: var(--r-md, 8px); padding: var(--sp-3); min-width: 420px; max-width: min(720px, 92vw); display: grid; gap: var(--sp-2); box-shadow: 0 12px 32px rgba(0, 0, 0, 0.18); }
.md-card h4 { margin: 0; font-size: 14px; }
.md-hint { margin: 0; font-size: 12px; color: var(--text-2); }
.md-card textarea { width: 100%; resize: vertical; font-size: 12px; }
.md-prompt { margin: 0; font-size: 13px; color: var(--text-2); overflow-wrap: anywhere; }
.md-card-actions { display: flex; justify-content: flex-end; gap: var(--sp-2); }
.grow { flex: 1; }
.mono { font-family: var(--mono, ui-monospace, monospace); }
</style>
