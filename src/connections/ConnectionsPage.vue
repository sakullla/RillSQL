<script setup lang="ts">
import { computed, onMounted, reactive, ref, watch } from "vue";
import { catalogConnections, refreshCatalog } from "../catalog/store";
import { tauriConnectionApi, type ConnectionApi, type EnvTagInput, type TestReport } from "./api";
import ConnIcon from "./ConnIcon.vue";
import EnvTagField from "./EnvTagField.vue";
import { pickSqliteFile, replacePaletteConnections } from "./index";
import { ContextMenu, type MenuItem } from "../ui";
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
const busy = ref(false);
const menuId = ref<string | null>(null);
const dialog = ref<"edit" | "delete" | "password" | "details" | null>(null);
const advancedOpen = ref(false);
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
const tags = ref<EnvTag[]>([
  { id: "dev", label: "开发", tone: "dev", position: 0 },
  { id: "test", label: "测试", tone: "test", position: 1 },
  { id: "prod", label: "生产", tone: "prod", position: 2 },
  { id: "local", label: "本地", tone: "local", position: 3 },
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

function openWorkspace(connection: SavedConnection) {
  const catalog = catalogConnections().find(item => item.id === connection.id);
  const database = catalog?.defaultDatabase ?? catalog?.databases[0]?.name ?? connection.database ?? (connection.engine === "sqlite" ? "main" : null);
  if (database == null) { emit("workspace"); return; }
  emit("query-target", { connectionId: connection.id, connectionName: connection.name, database, engine: connection.engine, status: "connected" });
}
function connectionState(id: string) {
  return statuses.value[id] ?? catalogConnections().find(item => item.id === id)?.status ?? "disconnected";
}
const connectedCount = computed(() => connections.value.filter(item => connectionState(item.id) === "connected").length);
const stateLabels: Record<string, string> = { connected: "已连接", connecting: "正在连接…", disconnected: "未连接", error: "连接失败", lost: "连接中断" };
const failedStage = computed(() => report.value?.stages.find((stage) => stage.status === "err") ?? null);

const rowMenu = ref<{ x: number; y: number; item: SavedConnection } | null>(null);
const rowMenuEl = ref<HTMLElement | null>(null);

function onCardContextMenu(event: MouseEvent, item: SavedConnection) {
  const node = event.target;
  if (!(node instanceof Element)) return;
  const card = node.closest<HTMLElement>(".conn-card");
  if (!card) return;
  // 先聚焦触发行：菜单关闭时 ContextMenu 会把焦点还回来，键盘用户不掉焦点。
  card.setAttribute("tabindex", "-1");
  card.focus();
  rowMenuEl.value = card;
  rowMenu.value = { x: Math.max(8, event.clientX), y: Math.max(8, event.clientY), item };
}

const rowMenuItems = computed<MenuItem[]>(() => {
  const item = rowMenu.value?.item;
  if (!item) return [];
  const state = connectionState(item.id);
  return [
    { kind: "item", id: "connect-or-open", label: state === "connected" ? "打开" : "连接", disabled: state === "connecting" },
    { kind: "item", id: "edit", label: "编辑配置…" },
    { kind: "item", id: "test", label: "测试连接" },
    { kind: "item", id: "copy", label: "复制配置", title: "不含密码" },
    { kind: "item", id: "workspace", label: "在工作区打开", disabled: state !== "connected" },
    { kind: "sep" },
    { kind: "item", id: "delete", label: "删除…", danger: true },
  ];
});

async function testFromRow(item: SavedConnection) {
  // 复用编辑弹窗里的 runTest：预填配置、打开弹窗、立即跑一次，结果在弹窗里看。
  openEditor(item);
  await runTest();
}

function copyConfig(item: SavedConnection) {
  const clean = Object.fromEntries(Object.entries(item).filter(([key]) => !/password|passphrase/i.test(key)));
  void navigator.clipboard.writeText(JSON.stringify(clean, null, 2)).catch(() => undefined);
}

function onRowMenuSelect(id: string) {
  const item = rowMenu.value?.item;
  if (!item) return;
  if (id === "connect-or-open") {
    if (connectionState(item.id) === "connected") openWorkspace(item);
    else requestConnect(item);
    return;
  }
  if (id === "edit") return openEditor(item);
  if (id === "test") return void testFromRow(item);
  if (id === "copy") return copyConfig(item);
  if (id === "workspace") return openWorkspace(item);
  if (id === "delete") return askDelete(item);
}

const advancedHint = computed(() =>
  draft.value.engine === "sqlite" ? "环境标签——本地文件不需要 TLS 或 SSH" : "TLS 加密、SSH 跳板机、环境标签——直连时通常保持默认即可",
);

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
    loadError.value = error instanceof Error ? error.message : "连接服务还没有挂到桌面命令上。";
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
    tagError.value = errorText(error, "环境标签没有读出来。");
  }
}

function usesAdvancedSettings(source: ConnectionDraft): boolean {
  return source.sshOn || source.tlsMode === "require" || source.tlsMode === "off" || source.caMode === "file" || Boolean(source.clientCert || source.clientKey);
}

function openEditor(existing?: SavedConnection, engine?: Engine) {
  report.value = null;
  fieldErrors.value = {};
  note.value = "";
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
    tagError.value = errorText(error, "没有保存标签。");
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
    tagError.value = errorText(error, "没有删除标签。");
  }
}

async function browseSqlite() {
  const file = await pickSqliteFile();
  if (!file) return;
  draft.value.filePath = file;
  if (!draft.value.name.trim()) draft.value.name = file.split(/[/\\]/).pop() ?? file;
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
  try {
    const outcome = await api.value.save(draft.value, currentSecrets());
    note.value = outcome.note ?? "";
    draft.value.id = outcome.connection.id;
    await reload();
    if (connectAfter) await connectSaved(outcome.connection, currentSecrets());
    else closeEditor();
  } catch (error) {
    note.value = error instanceof Error ? error.message : "没有保存。";
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
  try {
    const outcome = await api.value.save(draft.value, currentSecrets());
    draft.value.id = outcome.connection.id;
    if (outcome.note) note.value = outcome.note;
    report.value = await api.value.test(outcome.connection.id, currentSecrets());
    await reload();
  } catch (error) {
    note.value = error instanceof Error ? error.message : "测试没有开始。";
  } finally {
    busy.value = false;
  }
}

async function trustHostKey() {
  if (!draft.value.id || !report.value?.fingerprint) return;
  await api.value.confirmHostKey(draft.value.id, report.value.fingerprint);
  knownHostKey.value = report.value.fingerprint;
  await runTest();
}

function rejectHostKey() {
  if (!report.value) return;
  report.value = {
    ...report.value,
    status: "err",
    failStage: "未信任主机身份",
    hint: "没有发送任何凭据。",
    stages: report.value.stages.map((stage) =>
      stage.status === "wait" ? { ...stage, status: "err", message: "你拒绝了该主机身份" } : stage.status === "ok" ? stage : { ...stage, status: "skipped", message: "未执行" },
    ),
  };
}

async function forgetHostKey() {
  if (!draft.value.id) return;
  await api.value.forgetHostKey(draft.value.id);
  knownHostKey.value = null;
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
    note.value = error instanceof Error ? error.message : "连接失败。";
  }
}

function requestConnect(connection: SavedConnection) {
  menuId.value = null;
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
  const typed = { password: promptPassword.value, sshPassword: "", passphrase: "" };
  if (promptSave.value) {
    const outcome = await api.value.save({ ...draftFromSaved(target), savePassword: true }, typed);
    if (outcome.note) note.value = outcome.note;
    await reload();
  }
  closeDialog(otherDialog.value);
  dialog.value = null;
  await connectSaved(target, typed);
}

async function disconnect(connection: SavedConnection) {
  menuId.value = null;
  await api.value.disconnect(connection.id);
  statuses.value = { ...statuses.value, [connection.id]: "disconnected" };
  await refreshCatalog();
  delete statuses.value[connection.id];
}

function askDelete(connection: SavedConnection) {
  menuId.value = null;
  deleteTarget.value = connection;
  dialog.value = "delete";
  requestAnimationFrame(() => show(otherDialog.value));
}

async function confirmDelete() {
  const target = deleteTarget.value;
  if (!target) return;
  const outcome = await api.value.remove(target.id);
  note.value = outcome.databaseFileRemoved ? "连接配置已删除。" : `已删除连接配置“${target.name}”。数据库和其中的数据不受影响。`;
  closeDialog(otherDialog.value);
  dialog.value = null;
  await reload();
}

function showDetails(connection: SavedConnection) {
  menuId.value = null;
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
        <div><h1>连接管理</h1><p>一个入口，管理你的本地与远程数据库。</p></div>
        <div class="ph-actions"><button class="btn" type="button" @click="openEditor(undefined, 'sqlite')"><ConnIcon name="file" />打开 SQLite</button><button class="btn primary" type="button" data-action="conn-new" @click.stop="openEditor()"><ConnIcon name="plus" />新建连接</button></div>
      </div>
      <div class="connection-overview"><span><b>{{ connections.length }}</b> 个连接</span><span class="online-total"><i></i><b>{{ connectedCount }}</b> 个已连接</span><span class="grow"></span><span><ConnIcon name="lock" />凭据保存在本机</span></div>
      <div class="connection-filters">
        <label class="mini-search"><ConnIcon name="search" /><input v-model="query" type="search" placeholder="搜索名称、地址或文件…" aria-label="搜索连接" /></label>
        <select v-model="engineFilter" aria-label="按数据库类型筛选"><option value="all">全部数据库类型</option><option v-for="engine in ENGINES" :key="engine.id" :value="engine.id">{{ engine.label }}</option></select>
        <select v-model="envFilter" aria-label="按环境筛选"><option value="all">全部环境</option><option v-for="tag in tags" :key="tag.id" :value="tag.id">{{ tag.label }}</option><option value="none">无标签</option></select>
        <span class="grow"></span><span class="muted sm">{{ filtered.length }} 个连接</span>
      </div>
      <p v-if="loadError" class="page-note" role="alert">{{ loadError }}</p><p v-else-if="note" class="page-note" role="status">{{ note }}</p>
      <div class="conn-grid">
        <div class="connection-columns" aria-hidden="true"><span>连接 / 数据库</span><span>地址</span><span>传输与认证</span><span>状态 / 操作</span></div>
        <article v-for="item in filtered" :key="item.id" class="conn-card" :class="`st-${connectionState(item.id)}`" @contextmenu.prevent="onCardContextMenu($event, item)">
          <header>
            <span class="eng lg" :class="`eng-${item.engine}`">{{ engineMeta(item.engine).short }}</span>
            <div class="cc-title"><h3>{{ item.name }}</h3><p>{{ engineMeta(item.engine).label }} <span class="env" :class="`env-${envView(item.env).tone}`">{{ envView(item.env).label }}</span></p></div>
          </header>
          <div class="cc-endpoint"><p v-if="item.engine === 'sqlite'" class="cc-addr mono" :title="item.filePath ?? ''">{{ item.filePath }}</p><p v-else class="cc-addr mono" :title="`${item.host}:${item.port}`">{{ item.host }}:{{ item.port }}</p><small>{{ item.engine === 'sqlite' ? '本地文件' : item.database || '未指定默认数据库' }}</small></div>
          <div class="cc-badges">
            <template v-if="item.engine === 'sqlite'"><span class="badge"><ConnIcon name="file" />本地文件</span><span v-if="foreignKeys[item.id] != null" class="badge" :class="foreignKeys[item.id] ? 'ok' : 'warn'">外键约束{{ foreignKeys[item.id] ? '已开启' : '未开启' }}</span></template>
            <template v-else><span v-if="item.tls?.mode === 'verify'" class="badge ok"><ConnIcon name="shield" />TLS · 核对证书</span><span v-else-if="item.tls?.mode === 'require'" class="badge warn"><ConnIcon name="lock" />TLS · 不核对证书</span><span v-else class="badge warn"><ConnIcon name="unlock" />不加密</span><span v-if="item.ssh" class="badge"><ConnIcon name="tunnel" />SSH</span><small>{{ item.passwordSaved ? '密码已保存' : '每次询问密码' }}</small></template>
          </div>
          <footer><span class="conn-state" :class="`st-${connectionState(item.id)}`"><i></i>{{ stateLabels[connectionState(item.id)] }}</span><span class="grow"></span><button v-if="connectionState(item.id) === 'connected'" class="btn sm" type="button" @click.stop="openWorkspace(item)">打开</button><button v-else class="btn sm" type="button" data-action="conn-connect" :disabled="connectionState(item.id) === 'connecting'" @click.stop="requestConnect(item)"><ConnIcon name="plug" />连接</button><button class="icon-btn" type="button" :aria-label="`${item.name} 的更多操作`" data-action="conn-menu" @click.stop="menuId = menuId === item.id ? null : item.id"><ConnIcon name="more" /></button></footer>
          <div v-if="menuId === item.id" class="conn-menu" @click.stop><button type="button" @click="showDetails(item)"><ConnIcon name="info" />连接详情</button><button type="button" data-action="conn-edit" @click="openEditor(item)"><ConnIcon name="pencil" />编辑配置…</button><button v-if="connectionState(item.id) === 'connected'" type="button" @click="disconnect(item)"><ConnIcon name="unplug" />断开连接</button><button type="button" class="danger" data-action="conn-delete" @click="askDelete(item)"><ConnIcon name="trash" />删除配置…</button></div>
        </article>
        <div v-if="!filtered.length" class="empty"><ConnIcon name="database" /><h3>{{ connections.length ? '没有匹配的连接' : '连接你的第一个数据库' }}</h3><p>{{ connections.length ? '试试其他名称、数据库类型或环境。' : '支持 PostgreSQL、MySQL、MariaDB 和 SQLite。' }}</p></div>
        <button class="conn-card add" type="button" data-action="conn-new" @click="openEditor()"><ConnIcon name="plus" /><span>新建连接</span><small>PostgreSQL · MySQL · MariaDB · SQLite</small></button>
      </div>
      <div class="connection-note"><ConnIcon name="shield" /><span>连接配置只保存在本机。密码保存在操作系统凭据库；删除连接不会删除数据库或 SQLite 文件。</span></div>
    </div>

    <dialog ref="editDialog" class="modal modal-lg" @cancel.prevent="closeEditor">
      <div class="modal-head">
        <div class="modal-titles">
          <h2>{{ draft.id ? `编辑连接 · ${draft.name || draft.host || draft.filePath}` : "新建连接" }}</h2>
          <p class="modal-sub">配置只保存在本机；密码只写入系统凭据库</p>
        </div>
        <button class="icon-btn" type="button" aria-label="关闭" @click="closeEditor"><ConnIcon name="x" /></button>
      </div>
      <div class="modal-body">
        <div class="ce-engines" role="radiogroup" aria-label="数据库类型">
          <label v-for="engine in ENGINES" :key="engine.id" class="ce-engine" :class="{ on: draft.engine === engine.id, disabled: Boolean(draft.id && draft.engine !== engine.id) }">
            <input type="radio" name="cf-engine" :value="engine.id" :checked="draft.engine === engine.id" :disabled="Boolean(draft.id && draft.engine !== engine.id)" @change="switchEngine(engine.id)" />
            <span class="eng lg" :class="`eng-${engine.id}`">{{ engine.short }}</span>
            <span><b>{{ engine.label }}</b><small>{{ engine.id === "sqlite" ? "本地文件" : `默认端口 ${engine.port}` }}</small></span>
          </label>
        </div>

        <div v-if="draft.engine === 'sqlite'" class="form-grid">
          <label class="field full"><span>连接名称</span><input v-model="draft.name" placeholder="例如 本地笔记" /></label>
          <label class="field full">
            <span>数据库文件<em class="req">*</em></span>
            <div class="input-row">
              <input v-model="draft.filePath" class="mono" data-field="filePath" placeholder="选择已有的 .db / .sqlite 文件" :aria-invalid="Boolean(fieldErrors.filePath)" />
              <button class="btn" type="button" data-action="cf-browse" @click="browseSqlite"><ConnIcon name="file" />选择…</button>
            </div>
            <small v-if="fieldErrors.filePath" class="field-err">{{ fieldErrors.filePath }}</small>
            <small v-else>只打开已有文件。删除这个连接不会删除文件。</small>
          </label>
        </div>
        <div v-else class="form-grid">
          <label class="field full"><span>连接名称</span><input v-model="draft.name" placeholder="例如 Atlas" /></label>
          <label class="field"><span>主机<em class="req">*</em></span><input v-model="draft.host" class="mono" data-field="host" placeholder="db.example.internal" :aria-invalid="Boolean(fieldErrors.host)" /><small v-if="fieldErrors.host" class="field-err">{{ fieldErrors.host }}</small></label>
          <label class="field"><span>端口<em class="req">*</em></span><input v-model="draft.port" class="mono" data-field="port" :aria-invalid="Boolean(fieldErrors.port)" /><small v-if="fieldErrors.port" class="field-err">{{ fieldErrors.port }}</small></label>
          <label class="field"><span>数据库</span><input v-model="draft.database" class="mono" :placeholder="draft.engine === 'postgres' ? 'postgres' : '可选'" /></label>
          <label class="field"><span>用户名<em class="req">*</em></span><input v-model="draft.username" class="mono" data-field="username" :aria-invalid="Boolean(fieldErrors.username)" /><small v-if="fieldErrors.username" class="field-err">{{ fieldErrors.username }}</small></label>
          <label class="field full"><span>密码</span><input v-model="secrets.password" type="password" autocomplete="new-password" /><small>留空则继续使用系统凭据库里已保存的密码。密码不会写入普通配置。</small></label>
          <label class="check full"><input v-model="draft.savePassword" type="checkbox" /><span>把密码保存到系统凭据库<small>不勾选时，每次连接都会询问密码。</small></span></label>
        </div>
        <div v-if="draft.engine === 'sqlite'" class="callout callout-info"><ConnIcon name="info" /><div>SQLite 是本地文件，不需要 TLS 或 SSH。连接后会显示外键约束（foreign_keys）的实际状态——SQLite 默认不强制外键。</div></div>

        <details class="disclosure ce-advanced" data-section="advanced" :open="advancedOpen" @toggle="syncAdvanced">
          <summary>高级选项 <span class="muted">{{ advancedHint }}</span></summary>
          <div class="disclosure-body">
            <EnvTagField v-model="draft.env" :tags="tags" :error="tagError" @save="saveTag" @remove="removeTag" />
            <template v-if="draft.engine !== 'sqlite'">
              <section class="adv-sec">
                <h3>TLS 加密</h3>
                <p class="adv-note">保护密码和查询数据在传输中不被窃听。直连可信网络时通常保持默认即可。</p>
                <div class="radio-stack">
                  <label v-for="item in ([['verify', '要求加密，并核对证书和主机名', '推荐。主机名不符时直接失败。'], ['require', '要求加密，但不核对证书', '无法加密时不会退回明文连接。'], ['off', '不加密', '只适合本机或完全可信的网络。']] as [TlsMode, string, string][])" :key="item[0]" class="radio-card" :class="{ on: draft.tlsMode === item[0] }">
                    <input type="radio" name="cf-tls" :value="item[0]" :checked="draft.tlsMode === item[0]" @change="draft.tlsMode = item[0]" />
                    <span><b>{{ item[1] }}</b><small>{{ item[2] }}</small></span>
                  </label>
                </div>
                <div v-if="draft.tlsMode !== 'off'" class="form-grid">
                  <label class="field"><span>受信任的 CA</span><select v-model="draft.caMode"><option value="system">系统信任根</option><option value="file">指定 CA 文件</option></select></label>
                  <label v-if="draft.caMode === 'file'" class="field"><span>CA 证书文件</span><input v-model="draft.caFile" class="mono" data-field="caFile" /><small v-if="fieldErrors.caFile" class="field-err">{{ fieldErrors.caFile }}</small></label>
                  <label class="field"><span>客户端证书（可选）</span><input v-model="draft.clientCert" class="mono" /></label>
                  <label class="field"><span>客户端私钥（可选）</span><input v-model="draft.clientKey" class="mono" /></label>
                </div>
                <div v-if="draft.tlsMode === 'verify'" class="tls-name">
                  <ConnIcon name="shield" />
                  <div>
                    <b>证书按主机名 <code>{{ draft.host || "（填写主机后显示）" }}</code> 核对</b>
                    <small data-tls-copy>{{ tlsVerificationCopy(draft) }}</small>
                  </div>
                </div>
                <div v-if="draft.tlsMode !== 'off'" class="callout callout-warn"><ConnIcon name="info" /><div>核对失败时连接直接终止：不会自动降低验证级别，也不会退回不加密连接重试。</div></div>
                <div v-else class="callout callout-warn"><ConnIcon name="unlock" /><div>连接不加密：数据库密码和查询数据会以明文在网络上传输。</div></div>
              </section>
              <section class="adv-sec">
                <h3>SSH 跳板机</h3>
                <p class="adv-note">数据库无法从本机直接访问时，可以经过一台 SSH 服务器中转。</p>
                <label class="check"><input v-model="draft.sshOn" type="checkbox" data-field="sshOn" /><span><b>通过 SSH 跳板机连接</b><small>支持单跳。每条连接单独打开一条到数据库的通道。</small></span></label>
                <div v-if="draft.sshOn" class="form-grid">
                  <label class="field"><span>跳板机主机<em class="req">*</em></span><input v-model="draft.sshHost" class="mono" data-field="sshHost" /><small v-if="fieldErrors.sshHost" class="field-err">{{ fieldErrors.sshHost }}</small></label>
                  <label class="field"><span>SSH 端口</span><input v-model="draft.sshPort" class="mono" data-field="sshPort" /><small v-if="fieldErrors.sshPort" class="field-err">{{ fieldErrors.sshPort }}</small></label>
                  <label class="field"><span>SSH 用户<em class="req">*</em></span><input v-model="draft.sshUser" class="mono" data-field="sshUser" /><small v-if="fieldErrors.sshUser" class="field-err">{{ fieldErrors.sshUser }}</small></label>
                  <label class="field"><span>登录方式</span><span class="seg-radio"><label :class="{ on: draft.sshAuth === 'key' }"><input type="radio" name="cf-sshauth" value="key" :checked="draft.sshAuth === 'key'" @change="draft.sshAuth = 'key'" />私钥</label><label :class="{ on: draft.sshAuth === 'password' }"><input type="radio" name="cf-sshauth" value="password" :checked="draft.sshAuth === 'password'" @change="draft.sshAuth = 'password'" />密码</label></span></label>
                  <template v-if="draft.sshAuth === 'key'">
                    <label class="field"><span>私钥文件</span><input v-model="draft.sshKey" class="mono" /></label>
                    <label class="field"><span>私钥口令（如有）</span><input v-model="secrets.passphrase" type="password" autocomplete="new-password" /><small>只用于解锁私钥，不写入配置或日志。</small></label>
                  </template>
                  <label v-else class="field full"><span>SSH 密码</span><input v-model="secrets.sshPassword" type="password" autocomplete="new-password" /></label>
                </div>
                <div v-if="draft.sshOn" class="host-key">
                  <ConnIcon name="key" />
                  <div>
                    <b>跳板机身份</b>
                    <small v-if="knownHostKey">已信任指纹 <code>{{ knownHostKey }}</code>。服务器出示的指纹与此不同时，连接会被阻止。</small>
                    <small v-else>尚未记录。首次连接时会显示指纹，核对一致后再继续。</small>
                  </div>
                  <button v-if="knownHostKey" class="btn sm ghost" type="button" @click="forgetHostKey">忘记已保存的指纹</button>
                </div>
                <div v-if="draft.sshOn" class="route-box">
                  <span class="muted sm">连接路径</span>
                  <div class="route">
                    <span class="route-hop"><ConnIcon name="monitor" />本机</span>
                    <i class="route-line"></i>
                    <span class="route-hop"><ConnIcon name="tunnel" />{{ draft.sshHost || "跳板机" }}:{{ draft.sshPort }}</span>
                    <i class="route-line"></i>
                    <span class="route-hop"><ConnIcon name="database" />{{ draft.host || "数据库主机" }}:{{ draft.port || "?" }}</span>
                  </div>
                </div>
              </section>
            </template>
          </div>
        </details>

        <div v-if="report" class="test-panel" :class="report.status" role="status">
          <p v-if="report.status === 'ok'" class="test-line ok"><ConnIcon name="check" />连接成功{{ report.summary ? ` · ${report.summary}` : "" }}</p>
          <template v-else-if="report.status === 'wait'">
            <p class="test-line wait"><ConnIcon name="key" />第一次连接这台 SSH 跳板机，需要先确认它的身份</p>
            <div class="fp-card">
              <p>请通过可信渠道核对 <code>{{ draft.sshHost }}:{{ draft.sshPort }}</code> 的指纹，确认一致后再继续。不一致说明可能连到了别的机器。</p>
              <pre>{{ report.algorithm }}  {{ report.fingerprint }}</pre>
              <div class="fp-actions">
                <button class="btn sm" type="button" @click="rejectHostKey">不一致，停止</button>
                <button class="btn sm primary" type="button" @click="trustHostKey">指纹一致，信任并继续</button>
              </div>
            </div>
          </template>
          <template v-else>
            <p class="test-line err"><ConnIcon name="x" />连接失败：{{ report.failStage || "未知原因" }}</p>
            <p v-if="failedStage?.message" class="test-detail" data-test-fail>{{ failedStage.message }}</p>
            <p v-if="report.hint" class="test-detail">{{ report.hint }}</p>
            <details class="disclosure test-all">
              <summary>查看各阶段 <span class="muted">{{ report.stages.length }} 个步骤的完整结果</span></summary>
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
          <summary>「测试连接」会检查什么 <span class="muted">依次执行 {{ plannedStageLabels(draft).length }} 个步骤</span></summary>
          <div class="disclosure-body">
            <ol class="plan-steps">
              <li v-for="label in plannedStageLabels(draft)" :key="label">{{ label }}</li>
            </ol>
          </div>
        </details>
      </div>
      <div class="modal-foot">
        <button class="btn" type="button" data-action="cf-test" :disabled="busy" @click="runTest"><ConnIcon name="play" />测试连接</button>
        <span class="grow"></span>
        <button class="btn" type="button" @click="closeEditor">取消</button>
        <button class="btn" type="button" data-action="cf-save" :disabled="busy" @click="saveDraft(false)">保存</button>
        <button class="btn primary" type="button" :disabled="busy" @click="saveDraft(true)">保存并连接</button>
      </div>
    </dialog>

    <dialog ref="otherDialog" class="modal" :class="dialog === 'delete' ? 'modal-md' : 'modal-sm'" @cancel.prevent="dialog = null">
      <template v-if="dialog === 'delete' && deleteTarget">
        <div class="modal-head"><div class="modal-titles"><h2>删除连接「{{ deleteTarget.name }}」？</h2><p class="modal-sub">删除前请确认下面两条</p></div></div>
        <div class="modal-body">
          <ul class="facts">
            <li><ConnIcon name="check" />会删除：本机保存的这条连接配置{{ deleteTarget.passwordSaved ? "，以及系统凭据库里保存的对应密码" : "" }}。</li>
            <li><ConnIcon name="x" />不会删除：{{ deleteTarget.engine === "sqlite" ? `SQLite 文件 ${deleteTarget.filePath}` : "数据库本身和其中的任何数据" }}。</li>
          </ul>
        </div>
        <div class="modal-foot"><span class="grow"></span><button class="btn" type="button" @click="closeDialog(otherDialog)">取消</button><button class="btn danger" type="button" data-action="confirm-delete" @click="confirmDelete">删除配置</button></div>
      </template>
      <template v-else-if="dialog === 'password' && passwordTarget">
        <div class="modal-head"><div class="modal-titles"><h2>连接 {{ passwordTarget.name }}</h2><p class="modal-sub">这条连接没有保存密码，所以每次都会询问。</p></div></div>
        <div class="modal-body">
          <label class="field"><span>数据库密码</span><input v-model="promptPassword" type="password" autocomplete="current-password" /></label>
          <label class="check"><input v-model="promptSave" type="checkbox" /><span>保存到系统凭据库，以后不再询问</span></label>
        </div>
        <div class="modal-foot"><span class="grow"></span><button class="btn" type="button" @click="closeDialog(otherDialog)">取消</button><button class="btn primary" type="button" @click="submitPrompt">连接</button></div>
      </template>
      <template v-else-if="detailTarget">
        <div class="modal-head"><div class="modal-titles"><h2>{{ detailTarget.name }}</h2><p class="modal-sub">{{ engineMeta(detailTarget.engine).label }}</p></div></div>
        <div class="modal-body">
          <dl class="kv">
            <template v-if="detailTarget.engine === 'sqlite'">
              <dt>文件</dt><dd class="mono">{{ detailTarget.filePath }}</dd>
            </template>
            <template v-else>
              <dt>地址</dt><dd class="mono">{{ detailTarget.host }}:{{ detailTarget.port }}</dd>
              <dt>数据库</dt><dd>{{ detailTarget.database || "—" }}</dd>
              <dt>用户</dt><dd class="mono">{{ detailTarget.username }}</dd>
            </template>
            <dt>密码</dt><dd>{{ detailTarget.passwordSaved ? "保存在系统凭据库" : "每次连接时询问" }}</dd>
            <dt>传输</dt><dd>{{ detailTarget.engine === "sqlite" ? "本地文件，不经过网络" : detailTarget.tls?.mode === "verify" ? `加密连接，按主机名 ${detailTarget.tls.serverName} 核对证书` : detailTarget.tls?.mode === "require" ? "加密连接，不核对证书" : "不加密" }}</dd>
            <template v-if="detailTarget.ssh">
              <dt>中转</dt><dd class="mono">经 SSH 跳板机 {{ detailTarget.ssh.host }}:{{ detailTarget.ssh.port }}</dd>
            </template>
          </dl>
        </div>
        <div class="modal-foot"><button class="btn" type="button" @click="openEditor(detailTarget)">编辑配置</button><span class="grow"></span><button class="btn" type="button" @click="closeDialog(otherDialog)">关闭</button></div>
      </template>
    </dialog>

    <ContextMenu
      :open="Boolean(rowMenu)"
      :x="rowMenu?.x ?? 0"
      :y="rowMenu?.y ?? 0"
      :items="rowMenuItems"
      :label="rowMenu ? `${rowMenu.item.name} 的操作` : '操作'"
      :trigger="rowMenuEl"
      @close="rowMenu = null"
      @select="onRowMenuSelect"
    />
  </div>
</template>
