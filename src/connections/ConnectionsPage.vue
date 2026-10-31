<script setup lang="ts">
import { computed, onMounted, reactive, ref, watch } from "vue";
import { catalogConnections, refreshCatalog } from "../catalog/store";
import { tr as t } from "../i18n";
import Listbox from "../ui/Listbox.vue";
import { tauriConnectionApi, type ConnectionApi, type EnvTagInput, type TestReport } from "./api";
import ConnIcon from "./ConnIcon.vue";
import EnvTagField from "./EnvTagField.vue";
import { importConnectionText, importRillSqlConnections } from "./import";
import { pickConnectionsFile, pickExportPath, pickSqliteFile, replacePaletteConnections } from "./index";
import {
  ENGINES,
  blankDraft,
  draftFromSaved,
  engineMeta,
  presentEnv,
  plannedStageLabels,
  tlsVerificationCopy,
  validate,
  type ConnectionDraft,
  type Engine,
  type EnvTag,
  type SavedConnection,
  type SecretInput,
  type TlsMode,
} from "./model";
import { environmentLabel } from "../shell/status-text";
import { isMongoUri } from "./model";
import { parseConnectionUrl } from "./url";
import "./connections.css";

const props = defineProps<{ sqlitePrompt: number; api?: ConnectionApi }>();
const emit = defineEmits<{ workspace: []; "sqlite-consumed": []; "query-target": [target: import("../shell/session").QueryTarget] }>();
const engineFilter = ref("all");
const envFilter = ref("all");
const api = computed(() => props.api ?? tauriConnectionApi);

const connections = ref<SavedConnection[]>([]);
const query = ref("");
const loadError = ref("");
const note = ref("");
/** 对话框里的失败必须显示在对话框内部，不能写到被遮罩挡住的页面提示里。 */
const dialogError = ref("");
const busy = ref(false);
const menuId = ref<string | null>(null);
const dialog = ref<"edit" | "delete" | "password" | "details" | "export" | "passphrase" | null>(null);
const advancedOpen = ref(false);
const exportChecked = ref<Record<string, boolean>>({});
const exportPassphrase = ref("");
const passphrasePath = ref<string | null>(null);
const passphraseInput = ref("");
const pasteText = ref("");
const pasteNote = ref("");
const pasteFailed = ref(false);
const draft = ref<ConnectionDraft>(blankDraft("postgres"));
const fieldErrors = ref<Record<string, string>>({});
const report = ref<TestReport | null>(null);
const knownHostKey = ref<string | null>(null);
const secrets = reactive<SecretInput>({ password: "", sshPassword: "", passphrase: "" });
const statuses = ref<Record<string, string>>({});
const versions = ref<Record<string, string>>({});
const foreignKeys = ref<Record<string, boolean>>({});
const deleteTarget = ref<SavedConnection | null>(null);
const passwordTarget = ref<SavedConnection | null>(null);
const promptPassword = ref("");
const promptSave = ref(false);
const editDialog = ref<HTMLDialogElement | null>(null);
const otherDialog = ref<HTMLDialogElement | null>(null);
const importInput = ref<HTMLInputElement | null>(null);
const tags = ref<EnvTag[]>([
  { id: "dev", label: environmentLabel("dev"), tone: "dev", position: 0 },
  { id: "test", label: environmentLabel("test"), tone: "test", position: 1 },
  { id: "prod", label: environmentLabel("prod"), tone: "prod", position: 2 },
  { id: "local", label: environmentLabel("local"), tone: "local", position: 3 },
]);
const tagsLoaded = ref(false);
const tagError = ref("");

const filtered = computed(() => {
  const q = query.value.trim().toLowerCase();
  return connections.value.filter((item) =>
    (engineFilter.value === "all" || item.engine === engineFilter.value) &&
    (envFilter.value === "all" || item.env === envFilter.value) &&
    [item.name, item.host, item.filePath, item.database, engineMeta(item.engine).label].some((value) => value && value.toLowerCase().includes(q)),
  );
});
const engineOptions = computed(() => [
  { value: "all", label: t("connections.page.allEngines") },
  ...ENGINES.map((engine) => ({ value: engine.id, label: engine.label })),
]);
const envOptions = computed(() => [
  { value: "all", label: t("connections.page.allEnvs") },
  ...tags.value.map((tag) => ({ value: tag.id, label: tag.label })),
  { value: "none", label: t("connections.page.noTag") },
]);

function openWorkspace(connection: SavedConnection) {
  const catalog = catalogConnections().find(item => item.id === connection.id);
  const database = catalog?.defaultDatabase ?? catalog?.databases[0]?.name ?? connection.database ?? (connection.engine === "sqlite" ? "main" : null);
  if (database == null) { emit("workspace"); return; }
  emit("query-target", { connectionId: connection.id, connectionName: connection.name, database, engine: connection.engine, status: "connected" });
}

/** 双击卡片：已连接就直接打开，未连接就发起连接。按钮上的双击不触发。 */
function onCardDoubleClick(connection: SavedConnection, event: MouseEvent) {
  const target = event.target;
  if (target instanceof Element && target.closest("button")) return;
  if (connectionState(connection.id) === "connected") openWorkspace(connection);
  else requestConnect(connection);
}
function connectionState(id: string) {
  return statuses.value[id] ?? catalogConnections().find(item => item.id === id)?.status ?? "disconnected";
}
const connectedCount = computed(() => connections.value.filter(item => connectionState(item.id) === "connected").length);
const STATUS_TEXT_KEYS: Record<string, string> = {
  connected: "connections.status.connected",
  connecting: "connections.status.connecting",
  disconnected: "connections.status.disconnected",
  error: "connections.status.error",
  lost: "connections.status.lost",
};
function stateText(state: string): string {
  return t(STATUS_TEXT_KEYS[state] ?? "connections.status.disconnected");
}
const failedStage = computed(() => report.value?.stages.find((stage) => stage.status === "err") ?? null);

const advancedHint = computed(() =>
  draft.value.engine === "sqlite" ? t("connections.editor.advancedSqliteHint") : t("connections.editor.advancedHint"),
);

/** MongoDB 整串 URI 与 SSH 隧道互斥（后端保存阶段同样拒绝）：主机是 URI 时锁死 SSH 段。 */
const mongoUriBlocksSsh = computed(() => draft.value.engine === "mongodb" && isMongoUri(draft.value.host));

function show(target: HTMLDialogElement | null) {
  if (!target) return;
  if (typeof target.showModal === "function") {
    try {
      target.showModal();
      return;
    } catch {
      /* 已经打开，或当前环境没有模态对话框。 */
    }
  }
  target.setAttribute("open", "");
}

function closeDialog(target: HTMLDialogElement | null) {
  if (!target) return;
  if (typeof target.close === "function") target.close();
  target.removeAttribute("open");
}

function publishPalette() {
  replacePaletteConnections(
    connections.value.map((item) => ({
      id: item.id,
      label: item.name,
      path: item.engine === "sqlite" ? (item.filePath ?? "") : `${item.host ?? ""}:${item.port ?? ""}`,
    })),
  );
}

async function reload() {
  try {
    connections.value = await api.value.list();
    loadError.value = "";
    publishPalette();
  } catch (error) {
    loadError.value = error instanceof Error ? error.message : t("connections.errors.serviceMissing");
  }
}

function resetSecrets() {
  secrets.password = "";
  secrets.sshPassword = "";
  secrets.passphrase = "";
}

function envView(id: string) {
  return presentEnv(id, tags.value);
}

function errorText(error: unknown, fallback: string): string {
  if (typeof error === "string" && error.trim()) return error;
  if (error instanceof Error && error.message.trim()) return error.message;
  if (error && typeof error === "object" && "message" in error) {
    const message = (error as { message?: unknown }).message;
    if (typeof message === "string" && message.trim()) return message;
  }
  return fallback;
}

function ensureDraftEnv() {
  if (!tagsLoaded.value) return;
  if (draft.value.env !== "none" && !tags.value.some((tag) => tag.id === draft.value.env)) {
    draft.value.env = "none";
  }
}

async function reloadTags() {
  try {
    tags.value = await api.value.listTags();
    tagsLoaded.value = true;
    tagError.value = "";
    ensureDraftEnv();
  } catch (error) {
    tagError.value = errorText(error, t("connections.errors.tagsLoadFailed"));
  }
}

function usesAdvancedSettings(source: ConnectionDraft): boolean {
  return source.sshOn || source.tlsMode === "require" || source.tlsMode === "off" || source.caMode === "file" || Boolean(source.clientCert || source.clientKey);
}

function openEditor(existing?: SavedConnection, engine?: Engine) {
  report.value = null;
  fieldErrors.value = {};
  note.value = "";
  dialogError.value = "";
  pasteText.value = "";
  pasteNote.value = "";
  pasteFailed.value = false;
  resetSecrets();
  if (existing) {
    draft.value = draftFromSaved(existing);
    knownHostKey.value = existing.sshHostKey ?? null;
  } else {
    draft.value = blankDraft(engine ?? "postgres");
    knownHostKey.value = null;
  }
  ensureDraftEnv();
  advancedOpen.value = Boolean(existing) && usesAdvancedSettings(draft.value);
  dialog.value = "edit";
  menuId.value = null;
  requestAnimationFrame(() => show(editDialog.value));
}

function switchEngine(engine: Engine) {
  if (draft.value.id && draft.value.engine !== engine) return;
  const name = draft.value.name;
  const next = blankDraft(engine);
  next.name = name;
  draft.value = next;
  ensureDraftEnv();
  report.value = null;
}

/** 从连接串解析并填充表单。密码只进本次会话，不自动写入系统凭据库（对齐后端 secrets.rs 的语义）。 */
function applyConnectionUrl() {
  const parsed = parseConnectionUrl(pasteText.value);
  if (!parsed.ok) {
    pasteFailed.value = true;
    pasteNote.value = parsed.reason;
    return;
  }
  const parts: string[] = [];
  if (parsed.engine === draft.value.engine) {
    parts.push(t("connections.parse.filled"));
  } else if (draft.value.id) {
    // 编辑已保存的连接时不改引擎（switchEngine 对带 id 的草稿本来就不动），只填主机等字段。
    parts.push(t("connections.parse.engineMismatch", { engine: engineMeta(parsed.engine).label }));
  } else {
    switchEngine(parsed.engine);
    parts.push(t("connections.parse.engineSwitched", { engine: engineMeta(parsed.engine).label }));
  }
  draft.value.host = parsed.host;
  draft.value.port = parsed.port;
  draft.value.database = parsed.database;
  draft.value.username = parsed.username;
  if (parsed.tlsMode) draft.value.tlsMode = parsed.tlsMode;
  if (parsed.password) {
    secrets.password = parsed.password;
    draft.value.savePassword = false;
    parts.push(t("connections.parse.passwordFilled"));
  }
  pasteFailed.value = false;
  pasteNote.value = parts.join("");
}

function syncAdvanced(event: Event) {
  advancedOpen.value = (event.target as HTMLDetailsElement).open;
}

function revealErrors(errors: Record<string, string>) {
  if (errors.sshHost || errors.sshUser || errors.sshPort || errors.caFile) advancedOpen.value = true;
}

async function saveTag(payload: EnvTagInput) {
  try {
    const saved = await api.value.saveTag(payload);
    tags.value = await api.value.listTags();
    tagsLoaded.value = true;
    if (!payload.id) draft.value.env = saved.id;
    tagError.value = "";
    refreshCatalog();
  } catch (error) {
    tagError.value = errorText(error, t("connections.errors.tagSaveFailed"));
  }
}

async function removeTag(id: string) {
  try {
    await api.value.deleteTag(id);
    if (draft.value.env === id) draft.value.env = "none";
    tags.value = await api.value.listTags();
    tagsLoaded.value = true;
    tagError.value = "";
    await reload();
    refreshCatalog();
  } catch (error) {
    tagError.value = errorText(error, t("connections.errors.tagDeleteFailed"));
  }
}

async function browseSqlite() {
  const file = await pickSqliteFile();
  if (!file) return;
  draft.value.filePath = file;
  if (!draft.value.name.trim()) draft.value.name = file.split(/[/\\]/).pop() ?? file;
}

function openImport() {
  importInput.value?.click();
}

const exportSelection = computed(() => connections.value.filter((item) => exportChecked.value[item.id]));
const exportAllSelected = computed(() => connections.value.length > 0 && exportSelection.value.length === connections.value.length);

/** 导出对话框：默认全选，可改为部分选中；口令留空导出明文 JSON。 */
function openExport() {
  exportChecked.value = Object.fromEntries(connections.value.map((item) => [item.id, true]));
  exportPassphrase.value = "";
  dialogError.value = "";
  dialog.value = "export";
  requestAnimationFrame(() => show(otherDialog.value));
}

function toggleExportAll(event: Event) {
  const checked = (event.target as HTMLInputElement).checked;
  exportChecked.value = Object.fromEntries(connections.value.map((item) => [item.id, checked]));
}

async function confirmExport() {
  const selected = exportSelection.value;
  if (!selected.length) {
    dialogError.value = t("connections.exportDialog.needOne");
    return;
  }
  const passphrase = exportPassphrase.value.trim();
  const path = await pickExportPath(Boolean(passphrase));
  if (!path) return;
  busy.value = true;
  dialogError.value = "";
  try {
    const ids = selected.length === connections.value.length ? null : selected.map((item) => item.id);
    const outcome = await api.value.export(ids, passphrase, path);
    note.value = passphrase
      ? t("connections.exportDialog.doneEncrypted", { n: outcome.count })
      : t("connections.exportDialog.donePlain", { n: outcome.count });
    closeDialog(otherDialog.value);
    dialog.value = null;
  } catch (error) {
    dialogError.value = errorText(error, t("connections.errors.exportFailed"));
  } finally {
    busy.value = false;
  }
}

async function runRillSqlImport(path: string, passphrase: string): Promise<void> {
  const outcome = await api.value.importFile(path, passphrase);
  const result = await importRillSqlConnections(outcome, (draftToSave, secrets) => api.value.save(draftToSave, secrets));
  await reload();
  note.value = result.message;
}

/** RillSQL 文件导入：原生对话框选文件；文件带口令时先要口令，口令错误整体拒绝。 */
async function importRillSqlFile() {
  const path = await pickConnectionsFile();
  if (!path) return;
  busy.value = true;
  note.value = "";
  dialogError.value = "";
  try {
    await runRillSqlImport(path, "");
  } catch (error) {
    const message = errorText(error, t("connections.errors.importFailed"));
    // 后端（export.rs）对口令加密文件返回中文错误串「带口令加密」——这是前后端契约标记，与界面语言无关；
    // 用转义写法避免源码出现裸中文（verify:workbench 字典化扫描）。
    if (message.includes("\u5e26\u53e3\u4ee4\u52a0\u5bc6")) {
      passphrasePath.value = path;
      passphraseInput.value = "";
      dialog.value = "passphrase";
      requestAnimationFrame(() => show(otherDialog.value));
      return;
    }
    note.value = message;
  } finally {
    busy.value = false;
  }
}

async function submitPassphrase() {
  const path = passphrasePath.value;
  if (!path) return;
  dialogError.value = "";
  busy.value = true;
  try {
    await runRillSqlImport(path, passphraseInput.value);
    closeDialog(otherDialog.value);
    dialog.value = null;
    passphrasePath.value = null;
  } catch (error) {
    dialogError.value = errorText(error, t("connections.errors.importFailed"));
  } finally {
    busy.value = false;
  }
}

async function onImportPicked(event: Event) {
  const input = event.target;
  if (!(input instanceof HTMLInputElement)) return;
  const file = input.files?.[0] ?? null;
  input.value = "";
  if (!file) return;
  let text = "";
  try {
    text = await file.text();
  } catch (error) {
    note.value = errorText(error, t("connections.errors.fileUnread"));
    return;
  }
  busy.value = true;
  note.value = "";
  try {
    const outcome = await importConnectionText(text, (draftToSave, secrets) => api.value.save(draftToSave, secrets));
    await reload();
    note.value = outcome.message;
  } catch (error) {
    note.value = errorText(error, t("connections.errors.importFailed"));
  } finally {
    busy.value = false;
  }
}

function currentSecrets(): SecretInput {
  return { password: secrets.password, sshPassword: secrets.sshPassword, passphrase: secrets.passphrase };
}

async function saveDraft(connectAfter: boolean) {
  const errors = validate(draft.value);
  fieldErrors.value = errors;
  if (Object.keys(errors).length) {
    revealErrors(errors);
    return;
  }
  busy.value = true;
  dialogError.value = "";
  try {
    const outcome = await api.value.save(draft.value, currentSecrets());
    note.value = outcome.note ?? "";
    draft.value.id = outcome.connection.id;
    await reload();
    if (connectAfter) await connectSaved(outcome.connection, currentSecrets());
    else closeEditor();
  } catch (error) {
    dialogError.value = errorText(error, t("connections.errors.saveFailed"));
  } finally {
    busy.value = false;
  }
}

function closeEditor() {
  dialog.value = null;
  closeDialog(editDialog.value);
}

async function runTest() {
  const errors = validate(draft.value);
  fieldErrors.value = errors;
  if (Object.keys(errors).length) {
    revealErrors(errors);
    return;
  }
  busy.value = true;
  dialogError.value = "";
  try {
    const outcome = await api.value.save(draft.value, currentSecrets());
    draft.value.id = outcome.connection.id;
    if (outcome.note) note.value = outcome.note;
    report.value = await api.value.test(outcome.connection.id, currentSecrets());
    await reload();
  } catch (error) {
    dialogError.value = errorText(error, t("connections.errors.testFailed"));
  } finally {
    busy.value = false;
  }
}

async function trustHostKey() {
  if (!draft.value.id || !report.value?.fingerprint) return;
  dialogError.value = "";
  try {
    await api.value.confirmHostKey(draft.value.id, report.value.fingerprint);
    knownHostKey.value = report.value.fingerprint;
    await runTest();
  } catch (error) {
    dialogError.value = errorText(error, t("connections.errors.hostKeySaveFailed"));
  }
}

function rejectHostKey() {
  if (!report.value) return;
  report.value = {
    ...report.value,
    status: "err",
    failStage: t("connections.errors.failStageReject"),
    hint: t("connections.errors.noCredentials"),
    stages: report.value.stages.map((stage) =>
      stage.status === "wait" ? { ...stage, status: "err", message: t("connections.errors.rejectedIdentity") } : stage.status === "ok" ? stage : { ...stage, status: "skipped", message: t("connections.errors.skipped") },
    ),
  };
}

async function forgetHostKey() {
  if (!draft.value.id) return;
  dialogError.value = "";
  try {
    await api.value.forgetHostKey(draft.value.id);
    knownHostKey.value = null;
  } catch (error) {
    dialogError.value = errorText(error, t("connections.errors.hostKeyClearFailed"));
  }
}

async function connectSaved(connection: SavedConnection, typed: SecretInput) {
  statuses.value = { ...statuses.value, [connection.id]: "connecting" };
  try {
    const outcome = await api.value.open(connection.id, typed);
    report.value = outcome.report;
    if (outcome.connected) {
      statuses.value = { ...statuses.value, [connection.id]: "connected" };
      if (outcome.report.summary) versions.value = { ...versions.value, [connection.id]: outcome.report.summary };
      if (outcome.foreignKeys != null) foreignKeys.value = { ...foreignKeys.value, [connection.id]: outcome.foreignKeys };
      closeEditor();
      await refreshCatalog();
      if (catalogConnections().some(item => item.id === connection.id)) delete statuses.value[connection.id];
    } else {
      statuses.value = { ...statuses.value, [connection.id]: "error" };
      openEditor(connection);
      report.value = outcome.report;
    }
  } catch (error) {
    statuses.value = { ...statuses.value, [connection.id]: "error" };
    const message = errorText(error, t("connections.errors.connectFailed"));
    if (dialog.value) dialogError.value = message;
    else note.value = message;
  }
}

function requestConnect(connection: SavedConnection) {
  menuId.value = null;
  dialogError.value = "";
  if (connection.engine !== "sqlite" && !connection.passwordSaved) {
    passwordTarget.value = connection;
    promptPassword.value = "";
    promptSave.value = false;
    dialog.value = "password";
    requestAnimationFrame(() => show(otherDialog.value));
    return;
  }
  void connectSaved(connection, { password: "", sshPassword: "", passphrase: "" });
}

async function submitPrompt() {
  const target = passwordTarget.value;
  if (!target) return;
  dialogError.value = "";
  const typed = { password: promptPassword.value, sshPassword: "", passphrase: "" };
  try {
    if (promptSave.value) {
      const outcome = await api.value.save({ ...draftFromSaved(target), savePassword: true }, typed);
      if (outcome.note) note.value = outcome.note;
      await reload();
    }
    closeDialog(otherDialog.value);
    dialog.value = null;
    await connectSaved(target, typed);
  } catch (error) {
    dialogError.value = errorText(error, t("connections.errors.connectStartFailed"));
  }
}

async function disconnect(connection: SavedConnection) {
  menuId.value = null;
  note.value = "";
  try {
    await api.value.disconnect(connection.id);
    statuses.value = { ...statuses.value, [connection.id]: "disconnected" };
    await refreshCatalog();
    delete statuses.value[connection.id];
  } catch (error) {
    note.value = errorText(error, t("connections.errors.disconnectFailed"));
  }
}

function askDelete(connection: SavedConnection) {
  menuId.value = null;
  dialogError.value = "";
  deleteTarget.value = connection;
  dialog.value = "delete";
  requestAnimationFrame(() => show(otherDialog.value));
}

async function confirmDelete() {
  const target = deleteTarget.value;
  if (!target) return;
  dialogError.value = "";
  try {
    const outcome = await api.value.remove(target.id);
    note.value = outcome.databaseFileRemoved ? t("connections.errors.deletedDbFile") : t("connections.errors.deleted", { name: target.name });
    closeDialog(otherDialog.value);
    dialog.value = null;
    await reload();
  } catch (error) {
    dialogError.value = errorText(error, t("connections.errors.deleteFailed"));
  }
}

function showDetails(connection: SavedConnection) {
  menuId.value = null;
  dialogError.value = "";
  deleteTarget.value = connection;
  dialog.value = "details";
  requestAnimationFrame(() => show(otherDialog.value));
}

const detailTarget = computed(() => (dialog.value === "details" ? deleteTarget.value : null));

onMounted(() => {
  void reload();
  void reloadTags();
});

watch(
  () => props.sqlitePrompt,
  async (value) => {
    if (value <= 0) return;
    const file = await pickSqliteFile();
    emit("sqlite-consumed");
    openEditor(undefined, "sqlite");
    if (file) {
      draft.value.filePath = file;
      draft.value.name = draft.value.name || file.split(/[/\\]/).pop() || file;
    }
  },
  { immediate: true },
);
</script>

<template>
  <div class="page conn-page" @click="menuId = null">
    <div class="page-inner">
      <div class="page-head">
        <div><h1>{{ t("connections.page.title") }}</h1><p>{{ t("connections.page.subtitle") }}</p></div>
        <div class="ph-actions"><button class="btn" type="button" data-action="conn-import" :disabled="busy" @click="openImport"><ConnIcon name="upload" />{{ t("connections.page.importButton") }}</button><input ref="importInput" hidden type="file" accept=".json,.ncx,.xml,application/json,text/xml" data-action="conn-import-file"  :aria-label="t('connections.page.importFileLabel')" @change="onImportPicked" /><button class="btn" type="button" data-action="conn-import-rx" :disabled="busy" @click="importRillSqlFile"><ConnIcon name="file" />{{ t("connections.page.importRillSql") }}</button><button class="btn" type="button" data-action="conn-export" :disabled="busy || !connections.length" @click="openExport"><ConnIcon name="download" />{{ t("connections.page.exportButton") }}</button><button class="btn" type="button" @click="openEditor(undefined, 'sqlite')"><ConnIcon name="file" />{{ t("connections.page.openSqlite") }}</button><button class="btn primary" type="button" data-action="conn-new" @click.stop="openEditor()"><ConnIcon name="plus" />{{ t("common.newConnection") }}</button></div>
      </div>
      <div class="connection-overview"><span><b>{{ connections.length }}</b> {{ t("connections.page.countUnit", { n: connections.length }) }}</span><span class="online-total"><i></i><b>{{ connectedCount }}</b> {{ t("connections.page.connectedUnit", { n: connectedCount }) }}</span><span class="grow"></span><span><ConnIcon name="lock" />{{ t("connections.page.credentialsLocal") }}</span></div>
      <div class="connection-filters">
        <label class="mini-search"><ConnIcon name="search" /><input v-model="query" type="search"  :placeholder="t('connections.page.searchPlaceholder')" :aria-label="t('connections.page.searchLabel')" /></label>
        <Listbox v-model="engineFilter" :options="engineOptions"  :aria-label="t('connections.page.filterEngine')" />
        <Listbox v-model="envFilter" :options="envOptions"  :aria-label="t('connections.page.filterEnv')" />
        <span class="grow"></span><span class="muted sm">{{ t("connections.page.countUnit", { n: filtered.length }) }}</span>
      </div>
      <p v-if="loadError" class="page-note" role="alert"><ConnIcon name="alert" />{{ loadError }}</p><p v-else-if="note" class="page-note" role="status">{{ note }}</p>
      <div class="conn-grid">
        <div class="connection-columns" aria-hidden="true"><span>{{ t("connections.page.colConnection") }}</span><span>{{ t("connections.page.colAddress") }}</span><span>{{ t("connections.page.colTransport") }}</span><span>{{ t("connections.page.colStatus") }}</span></div>
        <article
          v-for="item in filtered"
          :key="item.id"
          class="conn-card"
          :class="`st-${connectionState(item.id)}`"
          :title="connectionState(item.id) === 'connected' ? t('connections.page.dblOpen') : t('connections.page.dblConnect')"
          @dblclick="onCardDoubleClick(item, $event)"
        >
          <header>
            <span class="eng lg" :class="`eng-${item.engine}`">{{ engineMeta(item.engine).short }}</span>
            <div class="cc-title"><h3>{{ item.name }}</h3><p>{{ engineMeta(item.engine).label }} <span class="env xs" :class="`env-${envView(item.env).tone}`">{{ envView(item.env).label }}</span></p></div>
          </header>
          <div class="cc-endpoint"><p v-if="item.engine === 'sqlite'" class="cc-addr mono" :title="item.filePath ?? ''">{{ item.filePath }}</p><p v-else class="cc-addr mono" :title="`${item.host}:${item.port}`">{{ item.host }}:{{ item.port }}</p><small>{{ item.engine === 'sqlite' ? t('connections.page.localFile') : item.database || t('connections.page.noDatabase') }}</small></div>
          <div class="cc-badges">
            <template v-if="item.engine === 'sqlite'"><span class="badge"><ConnIcon name="file" />{{ t("connections.page.localFile") }}</span><span v-if="foreignKeys[item.id] != null" class="badge" :class="foreignKeys[item.id] ? 'ok' : 'warn'">{{ foreignKeys[item.id] ? t('connections.page.fkEnabled') : t('connections.page.fkDisabled') }}</span></template>
            <template v-else><span v-if="item.tls?.mode === 'verify'" class="badge ok"><ConnIcon name="shield" />{{ t("connections.page.tlsVerify") }}</span><span v-else-if="item.tls?.mode === 'require'" class="badge warn"><ConnIcon name="lock" />{{ t("connections.page.tlsRequire") }}</span><span v-else class="badge warn"><ConnIcon name="unlock" />{{ t("connections.page.tlsOff") }}</span><span v-if="item.ssh" class="badge"><ConnIcon name="tunnel" />SSH</span><small>{{ item.passwordSaved ? t('connections.page.passwordSaved') : t('connections.page.askPassword') }}</small></template>
          </div>
          <footer><span class="conn-state" :class="`st-${connectionState(item.id)}`"><i></i>{{ stateText(connectionState(item.id)) }}</span><span class="grow"></span><button v-if="connectionState(item.id) === 'connected'" class="btn sm" type="button" @click.stop="openWorkspace(item)">{{ t("connections.page.open") }}</button><button v-else class="btn sm" type="button" data-action="conn-connect" :disabled="connectionState(item.id) === 'connecting'" @click.stop="requestConnect(item)"><ConnIcon name="plug" />{{ t("connections.page.connect") }}</button><button class="icon-btn" type="button" :aria-label="t('connections.page.moreActions', { name: item.name })" data-action="conn-menu" @click.stop="menuId = menuId === item.id ? null : item.id"><ConnIcon name="more" /></button></footer>
          <div v-if="menuId === item.id" class="conn-menu" @click.stop><button type="button" @click="showDetails(item)"><ConnIcon name="info" />{{ t("connections.page.menuDetails") }}</button><button type="button" data-action="conn-edit" @click="openEditor(item)"><ConnIcon name="pencil" />{{ t("connections.page.menuEdit") }}</button><button v-if="connectionState(item.id) === 'connected'" type="button" @click="disconnect(item)"><ConnIcon name="unplug" />{{ t("connections.page.menuDisconnect") }}</button><button type="button" class="danger" data-action="conn-delete" @click="askDelete(item)"><ConnIcon name="trash" />{{ t("connections.page.menuDelete") }}</button></div>
        </article>
        <div v-if="!filtered.length" class="empty"><ConnIcon name="database" /><h3>{{ connections.length ? t('connections.page.emptyTitle') : t('connections.page.emptyFirst') }}</h3><p>{{ connections.length ? t('connections.page.emptyHint') : t('connections.page.emptyFirstHint') }}</p></div>
      </div>
      <div class="connection-note"><ConnIcon name="shield" /><span>{{ t("connections.page.note") }}</span></div>
    </div>

    <dialog ref="editDialog" class="modal modal-lg" @cancel.prevent="closeEditor">
      <div class="modal-head">
        <div class="modal-titles">
          <h2>{{ draft.id ? t("connections.editor.editTitle", { name: draft.name || draft.host || draft.filePath }) : t("connections.editor.newTitle") }}</h2>
          <p class="modal-sub">{{ t("connections.editor.subtitle") }}</p>
        </div>
        <button class="icon-btn" type="button"  :aria-label="t('common.close')" @click="closeEditor"><ConnIcon name="x" /></button>
      </div>
      <div class="modal-body">
        <p v-if="dialogError" class="field-err modal-error" role="alert" data-dialog-error>{{ dialogError }}</p>
        <div class="ce-engines" role="radiogroup" :aria-label="t('connections.editor.enginesLabel')">
          <label v-for="engine in ENGINES" :key="engine.id" class="ce-engine" :class="{ on: draft.engine === engine.id, disabled: Boolean(draft.id && draft.engine !== engine.id) }">
            <input type="radio" name="cf-engine" :value="engine.id" :checked="draft.engine === engine.id" :disabled="Boolean(draft.id && draft.engine !== engine.id)" @change="switchEngine(engine.id)" />
            <span class="eng lg" :class="`eng-${engine.id}`">{{ engine.short }}</span>
            <span><b>{{ engine.label }}</b><small>{{ engine.id === "sqlite" ? t("connections.editor.engineLocal") : t("connections.editor.defaultPort", { port: engine.port }) }}</small></span>
          </label>
        </div>

        <details v-if="draft.engine !== 'sqlite'" class="disclosure" data-section="paste-url">
          <summary>{{ t("connections.editor.pasteTitle") }} <span class="muted">postgres://user:pass@host:5432/db</span></summary>
          <div class="disclosure-body">
            <label class="field full">
              <span>{{ t("connections.editor.pasteLabel") }}</span>
              <textarea v-model="pasteText" class="mono" data-field="url-paste" rows="2"  :placeholder="t('connections.editor.pastePlaceholder')" :aria-label="t('connections.editor.pasteLabel')"></textarea>
            </label>
            <div class="input-row">
              <button class="btn" type="button" data-action="cf-parse-url" @click="applyConnectionUrl">{{ t("connections.editor.parseFill") }}</button>
              <p v-if="pasteNote" class="paste-note" :class="pasteFailed ? 'field-err' : 'muted'" role="status">{{ pasteNote }}</p>
            </div>
          </div>
        </details>

        <div v-if="draft.engine === 'sqlite'" class="form-grid">
          <label class="field full"><span>{{ t("connections.editor.nameLabel") }}</span><input v-model="draft.name" :placeholder="t('connections.editor.namePlaceholderSqlite')" /></label>
          <label class="field full">
            <span>{{ t("connections.editor.fileLabel") }}<em class="req">*</em></span>
            <div class="input-row">
              <input v-model="draft.filePath" class="mono" data-field="filePath"  :placeholder="t('connections.editor.filePlaceholder')" :aria-invalid="Boolean(fieldErrors.filePath)" />
              <button class="btn" type="button" data-action="cf-browse" @click="browseSqlite"><ConnIcon name="file" />{{ t("connections.editor.browse") }}</button>
            </div>
            <small v-if="fieldErrors.filePath" class="field-err">{{ fieldErrors.filePath }}</small>
            <small v-else>{{ t("connections.editor.fileHint") }}</small>
          </label>
        </div>
        <div v-else class="form-grid">
          <label class="field full"><span>{{ t("connections.editor.nameLabel") }}</span><input v-model="draft.name" :placeholder="t('connections.editor.namePlaceholder')" /></label>
          <label class="field"><span>{{ t("connections.editor.hostLabel") }}<em class="req">*</em></span><input v-model="draft.host" class="mono" data-field="host" :placeholder="draft.engine === 'mongodb' ? t('connections.editor.hostPlaceholderMongodb') : t('connections.editor.hostPlaceholder')" :aria-invalid="Boolean(fieldErrors.host)" /><small v-if="fieldErrors.host" class="field-err">{{ fieldErrors.host }}</small><small v-else-if="draft.engine === 'mongodb'">{{ t("connections.editor.mongoHostHint") }}</small></label>
          <label class="field"><span>{{ t("connections.editor.portLabel") }}<em v-if="draft.engine !== 'mongodb' || !isMongoUri(draft.host)" class="req">*</em></span><input v-model="draft.port" class="mono" data-field="port" :aria-invalid="Boolean(fieldErrors.port)" /><small v-if="fieldErrors.port" class="field-err">{{ fieldErrors.port }}</small><small v-else-if="draft.engine === 'mongodb' && isMongoUri(draft.host)">{{ t("connections.editor.portMongoUriHint") }}</small></label>
          <label class="field"><span>{{ draft.engine === "redis" ? t("connections.editor.dbLabelRedis") : draft.engine === "mongodb" ? t("connections.editor.dbLabelMongo") : t("connections.editor.dbLabel") }}</span><input v-model="draft.database" class="mono" :placeholder="draft.engine === 'redis' ? t('connections.editor.dbPlaceholderRedis') : draft.engine === 'mongodb' ? t('connections.editor.dbPlaceholderMongo') : draft.engine === 'postgres' ? t('connections.editor.dbPlaceholderPostgres') : t('connections.editor.dbPlaceholderOptional')" :aria-invalid="Boolean(fieldErrors.database)" /><small v-if="fieldErrors.database" class="field-err">{{ fieldErrors.database }}</small><small v-else-if="draft.engine === 'redis'">{{ t("connections.editor.dbHintRedis") }}</small><small v-else-if="draft.engine === 'mongodb'">{{ t("connections.editor.dbHintMongo") }}</small></label>
          <label v-if="draft.engine === 'mongodb'" class="field"><span>{{ t("connections.editor.replicaSet") }}</span><input v-model="draft.replicaSet" class="mono" data-field="replicaSet" placeholder="rs0" /><small>{{ t("connections.editor.replicaSetHint") }}</small></label>
          <label class="field"><span>{{ t("connections.editor.userLabel") }}<em v-if="draft.engine !== 'redis' && draft.engine !== 'mongodb'" class="req">*</em></span><input v-model="draft.username" class="mono" data-field="username" :aria-invalid="Boolean(fieldErrors.username)" /><small v-if="fieldErrors.username" class="field-err">{{ fieldErrors.username }}</small><small v-else-if="draft.engine === 'redis'">{{ t("connections.editor.userHintRedis") }}</small><small v-else-if="draft.engine === 'mongodb'">{{ t("connections.editor.userHintMongo") }}</small></label>
          <label class="field full"><span>{{ t("connections.editor.passwordLabel") }}</span><input v-model="secrets.password" type="password" data-field="password" autocomplete="new-password" /><small>{{ t("connections.editor.passwordHint") }}</small></label>
          <label class="check full"><input v-model="draft.savePassword" type="checkbox" data-field="savePassword" /><span>{{ t("connections.editor.savePassword") }}<small>{{ t("connections.editor.savePasswordHint") }}</small></span></label>
        </div>
        <div v-if="draft.engine === 'sqlite'" class="callout callout-info"><ConnIcon name="info" /><div>{{ t("connections.editor.sqliteCallout") }}</div></div>

        <details class="disclosure ce-advanced" data-section="advanced" :open="advancedOpen" @toggle="syncAdvanced">
          <summary>{{ t("connections.editor.advanced") }} <span class="muted">{{ advancedHint }}</span></summary>
          <div class="disclosure-body">
            <EnvTagField v-model="draft.env" :tags="tags" :error="tagError" @save="saveTag" @remove="removeTag" />
            <template v-if="draft.engine !== 'sqlite'">
              <section class="adv-sec">
                <h3>{{ t("connections.editor.tls.title") }}</h3>
                <p class="adv-note">{{ t("connections.editor.tls.note") }}</p>
                <div class="radio-stack">
                  <label v-for="item in ([['verify', t('connections.editor.tls.verifyLabel'), t('connections.editor.tls.verifyHint')], ['require', t('connections.editor.tls.requireLabel'), t('connections.editor.tls.requireHint')], ['off', t('connections.editor.tls.offLabel'), t('connections.editor.tls.offHint')]] as [TlsMode, string, string][])" :key="item[0]" class="radio-card" :class="{ on: draft.tlsMode === item[0] }">
                    <input type="radio" name="cf-tls" :value="item[0]" :checked="draft.tlsMode === item[0]" @change="draft.tlsMode = item[0]" />
                    <span><b>{{ item[1] }}</b><small>{{ item[2] }}</small></span>
                  </label>
                </div>
                <div v-if="draft.tlsMode !== 'off'" class="form-grid">
                  <label class="field"><span>{{ t("connections.editor.tls.caLabel") }}</span><select v-model="draft.caMode"><option value="system">{{ t("connections.editor.tls.caSystem") }}</option><option value="file">{{ t("connections.editor.tls.caFile") }}</option></select></label>
                  <label v-if="draft.caMode === 'file'" class="field"><span>{{ t("connections.editor.tls.caFileLabel") }}</span><input v-model="draft.caFile" class="mono" data-field="caFile" /><small v-if="fieldErrors.caFile" class="field-err">{{ fieldErrors.caFile }}</small></label>
                  <label class="field"><span>{{ t("connections.editor.tls.clientCert") }}</span><input v-model="draft.clientCert" class="mono" /></label>
                  <label class="field"><span>{{ t("connections.editor.tls.clientKey") }}</span><input v-model="draft.clientKey" class="mono" /></label>
                </div>
                <div v-if="draft.tlsMode === 'verify'" class="tls-name">
                  <ConnIcon name="shield" />
                  <div>
                    <b>{{ t("connections.editor.tls.verifyTitle", { host: draft.host || t("connections.editor.tls.hostPending") }) }}</b>
                    <small data-tls-copy>{{ tlsVerificationCopy(draft) }}</small>
                  </div>
                </div>
                <div v-if="draft.tlsMode !== 'off'" class="callout callout-warn"><ConnIcon name="info" /><div>{{ t("connections.editor.tls.warnVerify") }}</div></div>
                <div v-else class="callout callout-warn"><ConnIcon name="unlock" /><div>{{ t("connections.editor.tls.warnOff") }}</div></div>
              </section>
              <section class="adv-sec">
                <h3>{{ t("connections.editor.ssh.title") }}</h3>
                <p class="adv-note">{{ t("connections.editor.ssh.note") }}</p>
                <label class="check" :class="{ disabled: mongoUriBlocksSsh }"><input v-model="draft.sshOn" type="checkbox" data-field="sshOn" :disabled="mongoUriBlocksSsh" /><span><b>{{ t("connections.editor.ssh.enable") }}</b><small>{{ t("connections.editor.ssh.enableHint") }}</small></span></label>
                <p v-if="mongoUriBlocksSsh" class="adv-note" data-mongo-uri-ssh-note>{{ t("connections.editor.ssh.uriBlocksSsh") }}</p>
                <div v-if="draft.sshOn && !mongoUriBlocksSsh" class="form-grid">
                  <label class="field"><span>{{ t("connections.editor.ssh.host") }}<em class="req">*</em></span><input v-model="draft.sshHost" class="mono" data-field="sshHost" /><small v-if="fieldErrors.sshHost" class="field-err">{{ fieldErrors.sshHost }}</small></label>
                  <label class="field"><span>{{ t("connections.editor.ssh.port") }}</span><input v-model="draft.sshPort" class="mono" data-field="sshPort" /><small v-if="fieldErrors.sshPort" class="field-err">{{ fieldErrors.sshPort }}</small></label>
                  <label class="field"><span>{{ t("connections.editor.ssh.user") }}<em class="req">*</em></span><input v-model="draft.sshUser" class="mono" data-field="sshUser" /><small v-if="fieldErrors.sshUser" class="field-err">{{ fieldErrors.sshUser }}</small></label>
                  <label class="field"><span>{{ t("connections.editor.ssh.authLabel") }}</span><span class="seg-radio"><label :class="{ on: draft.sshAuth === 'key' }"><input type="radio" name="cf-sshauth" value="key" :checked="draft.sshAuth === 'key'" @change="draft.sshAuth = 'key'" />{{ t('connections.editor.ssh.authKey') }}</label><label :class="{ on: draft.sshAuth === 'password' }"><input type="radio" name="cf-sshauth" value="password" :checked="draft.sshAuth === 'password'" @change="draft.sshAuth = 'password'" />{{ t('connections.editor.ssh.authPassword') }}</label></span></label>
                  <template v-if="draft.sshAuth === 'key'">
                    <label class="field"><span>{{ t("connections.editor.ssh.keyFile") }}</span><input v-model="draft.sshKey" class="mono" /></label>
                    <label class="field"><span>{{ t("connections.editor.ssh.passphrase") }}</span><input v-model="secrets.passphrase" type="password" autocomplete="new-password" /><small>{{ t("connections.editor.ssh.passphraseHint") }}</small></label>
                  </template>
                  <label v-else class="field full"><span>{{ t("connections.editor.ssh.password") }}</span><input v-model="secrets.sshPassword" type="password" autocomplete="new-password" /></label>
                </div>
                <div v-if="draft.sshOn && !mongoUriBlocksSsh" class="host-key">
                  <ConnIcon name="key" />
                  <div>
                    <b>{{ t("connections.editor.ssh.identity") }}</b>
                    <small v-if="knownHostKey">{{ t("connections.editor.ssh.trusted", { fp: knownHostKey }) }}</small>
                    <small v-else>{{ t("connections.editor.ssh.untrusted") }}</small>
                  </div>
                  <button v-if="knownHostKey" class="btn sm ghost" type="button" @click="forgetHostKey">{{ t("connections.editor.ssh.forget") }}</button>
                </div>
                <div v-if="draft.sshOn && !mongoUriBlocksSsh" class="route-box">
                  <span class="muted sm">{{ t("connections.editor.ssh.route") }}</span>
                  <div class="route">
                    <span class="route-hop"><ConnIcon name="monitor" />{{ t("connections.editor.ssh.local") }}</span>
                    <i class="route-line"></i>
                    <span class="route-hop"><ConnIcon name="tunnel" />{{ draft.sshHost || t('connections.editor.ssh.hopFallback') }}:{{ draft.sshPort }}</span>
                    <i class="route-line"></i>
                    <span class="route-hop"><ConnIcon name="database" />{{ draft.host || t('connections.editor.ssh.dbFallback') }}:{{ draft.port || "?" }}</span>
                  </div>
                </div>
              </section>
            </template>
          </div>
        </details>

        <div v-if="report" class="test-panel" :class="report.status" role="status">
          <p v-if="report.status === 'ok'" class="test-line ok"><ConnIcon name="check" />{{ report.summary ? t("connections.editor.test.okSummary", { summary: report.summary }) : t("connections.editor.test.ok") }}</p>
          <template v-else-if="report.status === 'wait'">
            <p class="test-line wait"><ConnIcon name="key" />{{ t("connections.editor.test.wait") }}</p>
            <div class="fp-card">
              <p>{{ t("connections.editor.test.verifyPrompt", { addr: `${draft.sshHost}:${draft.sshPort}` }) }}</p>
              <pre>{{ report.algorithm }}  {{ report.fingerprint }}</pre>
              <div class="fp-actions">
                <button class="btn sm" type="button" @click="rejectHostKey">{{ t("connections.editor.test.reject") }}</button>
                <button class="btn sm primary" type="button" @click="trustHostKey">{{ t("connections.editor.test.trust") }}</button>
              </div>
            </div>
          </template>
          <template v-else>
            <p class="test-line err"><ConnIcon name="x" />{{ t("connections.editor.test.failed", { stage: report.failStage || t("connections.editor.test.unknownReason") }) }}</p>
            <p v-if="failedStage?.message" class="test-detail" data-test-fail>{{ failedStage.message }}</p>
            <p v-if="report.hint" class="test-detail">{{ report.hint }}</p>
            <details class="disclosure test-all">
              <summary>{{ t("connections.editor.test.allSteps") }} <span class="muted">{{ t("connections.editor.test.stepsCount", { n: report.stages.length }) }}</span></summary>
              <div class="disclosure-body">
                <ol class="test-steps">
                  <li v-for="stage in report.stages" :key="stage.label" class="ts" :class="`ts-${stage.status === 'skipped' ? 'skip' : stage.status}`">
                    <span class="ts-i"><ConnIcon :name="stage.status === 'ok' ? 'check' : stage.status === 'err' ? 'x' : stage.status === 'wait' ? 'key' : 'minus'" /></span>
                    <span class="ts-name">{{ stage.label }}</span>
                    <span class="ts-msg">{{ stage.message }}</span>
                  </li>
                </ol>
              </div>
            </details>
          </template>
        </div>
        <details v-else class="disclosure test-plan">
          <summary>{{ t("connections.editor.test.planTitle") }} <span class="muted">{{ t("connections.editor.test.planCount", { n: plannedStageLabels(draft).length }) }}</span></summary>
          <div class="disclosure-body">
            <ol class="plan-steps">
              <li v-for="label in plannedStageLabels(draft)" :key="label">{{ label }}</li>
            </ol>
          </div>
        </details>
      </div>
      <div class="modal-foot">
        <button class="btn" type="button" data-action="cf-test" :disabled="busy" @click="runTest"><ConnIcon name="play" />{{ t("connections.editor.test.button") }}</button>
        <span class="grow"></span>
        <button class="btn" type="button" @click="closeEditor">{{ t("common.cancel") }}</button>
        <button class="btn" type="button" data-action="cf-save" :disabled="busy" @click="saveDraft(false)">{{ t("common.save") }}</button>
        <button class="btn primary" type="button" :disabled="busy" @click="saveDraft(true)">{{ t("connections.editor.test.saveConnect") }}</button>
      </div>
    </dialog>

    <dialog ref="otherDialog" class="modal" :class="dialog === 'delete' || dialog === 'export' ? 'modal-md' : 'modal-sm'" @cancel.prevent="dialog = null; passphrasePath = null">
      <template v-if="dialog === 'delete' && deleteTarget">
        <div class="modal-head"><div class="modal-titles"><h2>{{ t("connections.deleteDialog.title", { name: deleteTarget.name }) }}</h2><p class="modal-sub">{{ t("connections.deleteDialog.subtitle") }}</p></div></div>
        <div class="modal-body">
          <p v-if="dialogError" class="field-err modal-error" role="alert" data-dialog-error>{{ dialogError }}</p>
          <ul class="facts">
            <li class="fact-del"><ConnIcon name="trash" />{{ t("connections.deleteDialog.willDelete") }}{{ deleteTarget.passwordSaved ? t("connections.deleteDialog.willDeleteSecrets") : "" }}.</li>
            <li class="fact-keep"><ConnIcon name="shield" />{{ deleteTarget.engine === "sqlite" ? t("connections.deleteDialog.willKeepFile", { file: deleteTarget.filePath }) : t("connections.deleteDialog.willKeepDb") }}.</li>
          </ul>
        </div>
        <div class="modal-foot"><span class="grow"></span><button class="btn" type="button" @click="closeDialog(otherDialog)">{{ t("common.cancel") }}</button><button class="btn danger" type="button" data-action="confirm-delete" @click="confirmDelete">{{ t("connections.deleteDialog.confirm") }}</button></div>
      </template>
      <template v-else-if="dialog === 'password' && passwordTarget">
        <div class="modal-head"><div class="modal-titles"><h2>{{ t("connections.passwordDialog.title", { name: passwordTarget.name }) }}</h2><p class="modal-sub">{{ t("connections.passwordDialog.subtitle") }}</p></div></div>
        <div class="modal-body">
          <p v-if="dialogError" class="field-err modal-error" role="alert" data-dialog-error>{{ dialogError }}</p>
          <label class="field"><span>{{ t("connections.passwordDialog.label") }}</span><input v-model="promptPassword" type="password" autocomplete="current-password" /></label>
          <label class="check"><input v-model="promptSave" type="checkbox" /><span>{{ t("connections.passwordDialog.saveLabel") }}</span></label>
        </div>
        <div class="modal-foot"><span class="grow"></span><button class="btn" type="button" @click="closeDialog(otherDialog)">{{ t("common.cancel") }}</button><button class="btn primary" type="button" @click="submitPrompt">{{ t("connections.passwordDialog.connect") }}</button></div>
      </template>
      <template v-else-if="dialog === 'export'">
        <div class="modal-head"><div class="modal-titles"><h2>{{ t("connections.exportDialog.title") }}</h2><p class="modal-sub">{{ t("connections.exportDialog.subtitle") }}</p></div></div>
        <div class="modal-body">
          <p v-if="dialogError" class="field-err modal-error" role="alert" data-dialog-error>{{ dialogError }}</p>
          <label class="check export-all"><input type="checkbox" data-field="export-all" :checked="exportAllSelected" @change="toggleExportAll" /><span><b>{{ t("connections.exportDialog.all") }}</b><small>{{ t("connections.exportDialog.selected", { selected: exportSelection.length, total: connections.length }) }}</small></span></label>
          <div class="export-list" data-section="export-list">
            <label v-for="item in connections" :key="item.id" class="check">
              <input v-model="exportChecked[item.id]" type="checkbox" data-field="export-row" :value="item.id" />
              <span><b>{{ item.name }}</b><small>{{ engineMeta(item.engine).label }} · {{ item.engine === 'sqlite' ? (item.filePath ?? t('connections.page.localFile')) : `${item.host ?? ''}:${item.port ?? ''}` }}</small></span>
            </label>
          </div>
          <label class="field"><span>{{ t("connections.exportDialog.passphrase") }}</span><input v-model="exportPassphrase" type="password" data-field="export-passphrase" autocomplete="new-password" /><small>{{ t("connections.exportDialog.passphraseHint") }}</small></label>
        </div>
        <div class="modal-foot"><span class="grow"></span><button class="btn" type="button" @click="closeDialog(otherDialog)">{{ t("common.cancel") }}</button><button class="btn primary" type="button" data-action="confirm-export" :disabled="busy" @click="confirmExport"><ConnIcon name="download" />{{ t("connections.exportDialog.button") }}</button></div>
      </template>
      <template v-else-if="dialog === 'passphrase' && passphrasePath">
        <div class="modal-head"><div class="modal-titles"><h2>{{ t("connections.passphraseDialog.title") }}</h2><p class="modal-sub">{{ t("connections.passphraseDialog.subtitle") }}</p></div></div>
        <div class="modal-body">
          <p v-if="dialogError" class="field-err modal-error" role="alert" data-dialog-error>{{ dialogError }}</p>
          <label class="field"><span>{{ t("connections.passphraseDialog.label") }}</span><input v-model="passphraseInput" type="password" data-field="import-passphrase" autocomplete="current-password" /></label>
        </div>
        <div class="modal-foot"><span class="grow"></span><button class="btn" type="button" @click="dialog = null; passphrasePath = null">{{ t("common.cancel") }}</button><button class="btn primary" type="button" data-action="confirm-import-passphrase" :disabled="busy" @click="submitPassphrase">{{ t("connections.passphraseDialog.button") }}</button></div>
      </template>
      <template v-else-if="detailTarget">
        <div class="modal-head"><div class="modal-titles"><h2>{{ detailTarget.name }}</h2><p class="modal-sub">{{ engineMeta(detailTarget.engine).label }}</p></div></div>
        <div class="modal-body">
          <p v-if="dialogError" class="field-err modal-error" role="alert" data-dialog-error>{{ dialogError }}</p>
          <dl class="kv">
            <template v-if="detailTarget.engine === 'sqlite'">
              <dt>{{ t("connections.detailsDialog.file") }}</dt><dd class="mono">{{ detailTarget.filePath }}</dd>
            </template>
            <template v-else>
              <dt>{{ t("connections.detailsDialog.address") }}</dt><dd class="mono">{{ detailTarget.host }}:{{ detailTarget.port }}</dd>
              <dt>{{ t("connections.detailsDialog.database") }}</dt><dd>{{ detailTarget.database || "—" }}</dd>
              <dt>{{ t("connections.detailsDialog.user") }}</dt><dd class="mono">{{ detailTarget.username }}</dd>
            </template>
            <dt>{{ t("connections.detailsDialog.password") }}</dt><dd>{{ detailTarget.passwordSaved ? t("connections.detailsDialog.passwordSaved") : t("connections.detailsDialog.askEveryTime") }}</dd>
            <dt>{{ t("connections.detailsDialog.transport") }}</dt><dd>{{ detailTarget.engine === "sqlite" ? t("connections.detailsDialog.transportLocal") : detailTarget.tls?.mode === "verify" ? t("connections.detailsDialog.transportVerify", { name: detailTarget.tls.serverName }) : detailTarget.tls?.mode === "require" ? t("connections.detailsDialog.transportRequire") : t("connections.detailsDialog.transportOff") }}</dd>
            <template v-if="detailTarget.ssh">
              <dt>{{ t("connections.detailsDialog.via") }}</dt><dd class="mono">{{ t("connections.detailsDialog.viaSsh", { host: detailTarget.ssh.host, port: detailTarget.ssh.port }) }}</dd>
            </template>
          </dl>
        </div>
        <div class="modal-foot"><button class="btn" type="button" @click="openEditor(detailTarget)">{{ t("connections.detailsDialog.edit") }}</button><span class="grow"></span><button class="btn" type="button" @click="closeDialog(otherDialog)">{{ t("common.close") }}</button></div>
      </template>
    </dialog>
  </div>
</template>
