<script setup lang="ts">
import { computed, onMounted, reactive, ref, watch } from "vue";
import { tauriConnectionApi, type ConnectionApi, type TestReport } from "./api";
import ConnIcon from "./ConnIcon.vue";
import { pickSqliteFile, replacePaletteConnections } from "./index";
import {
  ENGINES,
  ENVS,
  blankDraft,
  draftFromSaved,
  engineMeta,
  envLabel,
  plannedStageLabels,
  tlsVerificationCopy,
  validate,
  type ConnectionDraft,
  type Engine,
  type EnvLabel,
  type SavedConnection,
  type SecretInput,
  type TlsMode,
} from "./model";
import "./connections.css";

const props = defineProps<{ sqlitePrompt: number; api?: ConnectionApi }>();
const api = computed(() => props.api ?? tauriConnectionApi);

const connections = ref<SavedConnection[]>([]);
const query = ref("");
const loadError = ref("");
const note = ref("");
const busy = ref(false);
const menuId = ref<string | null>(null);
const dialog = ref<"edit" | "delete" | "password" | "details" | null>(null);
const section = ref<"general" | "tls" | "ssh">("general");
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

const filtered = computed(() => {
  const q = query.value.trim().toLowerCase();
  if (!q) return connections.value;
  return connections.value.filter((item) =>
    [item.name, item.host, item.filePath, item.database, engineMeta(item.engine).label].some((value) => value && value.toLowerCase().includes(q)),
  );
});

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
  section.value = "general";
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
  report.value = null;
}

function chooseSection(next: "general" | "tls" | "ssh") {
  section.value = next;
}

function setEnv(value: string) {
  draft.value.env = value as EnvLabel;
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
    if (errors.host || errors.port || errors.username) section.value = "general";
    else if (errors.sshHost || errors.sshUser || errors.sshPort) section.value = "ssh";
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
  if (Object.keys(errors).length) return;
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
    } else {
      statuses.value = { ...statuses.value, [connection.id]: "error" };
      dialog.value = "edit";
      show(editDialog.value);
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
  note.value = outcome.databaseFileRemoved ? "配置已删除。" : `已删除连接配置“${target.name}”。数据库未受影响。`;
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
});

watch(
  () => props.sqlitePrompt,
  async (value) => {
    if (value <= 0) return;
    const file = await pickSqliteFile();
    openEditor(undefined, "sqlite");
    if (file) {
      draft.value.filePath = file;
      draft.value.name = draft.value.name || file.split(/[/\\]/).pop() || file;
    }
  },
);
</script>

<template>
  <div class="page conn-page" @click="menuId = null">
    <div class="page-inner">
      <div class="page-head">
        <div>
          <div class="eyebrow"><ConnIcon name="database" />连接</div>
          <h1>连接管理</h1>
          <p>连接配置只保存在本机。选择保存密码时写入操作系统凭据库，不会出现在配置文件或日志中。删除配置不会删除数据库或 SQLite 文件。</p>
        </div>
        <div class="ph-actions">
          <label class="mini-search">
            <ConnIcon name="search" />
            <input v-model="query" placeholder="搜索名称、主机或文件" aria-label="搜索连接" />
          </label>
          <button class="btn primary" type="button" data-action="conn-new" @click.stop="openEditor()">
            <ConnIcon name="plus" />新建连接
          </button>
        </div>
      </div>
      <p v-if="loadError" class="page-note">{{ loadError }}</p>
      <p v-else-if="note" class="page-note">{{ note }}</p>
      <div class="conn-grid">
        <article v-for="item in filtered" :key="item.id" class="conn-card" :class="`st-${statuses[item.id] ?? 'disconnected'}`">
          <header>
            <span class="eng lg" :class="`eng-${item.engine}`">{{ engineMeta(item.engine).short }}</span>
            <div class="cc-title">
              <h3>{{ item.name }}</h3>
              <p>{{ versions[item.id] || engineMeta(item.engine).label }}</p>
            </div>
            <span class="env" :class="`env-${item.env}`">{{ envLabel(item.env) }}</span>
          </header>
          <dl v-if="item.engine === 'sqlite'" class="cc-dl">
            <dt>文件</dt>
            <dd class="mono" :title="item.filePath ?? ''">{{ item.filePath }}</dd>
          </dl>
          <dl v-else class="cc-dl">
            <dt>地址</dt>
            <dd class="mono">{{ item.host }}:{{ item.port }}</dd>
            <dt>数据库</dt>
            <dd class="mono">{{ item.database || "—" }}</dd>
            <dt>用户</dt>
            <dd class="mono">{{ item.username }}</dd>
          </dl>
          <div class="cc-chips">
            <template v-if="item.engine === 'sqlite'">
              <span class="sec-chip"><ConnIcon name="file" />本地文件</span>
              <span class="sec-chip" :class="foreignKeys[item.id] ? '' : 'warn'">foreign_keys {{ foreignKeys[item.id] ? "ON" : "OFF" }}</span>
            </template>
            <template v-else>
              <span v-if="item.tls?.mode === 'verify'" class="sec-chip ok"><ConnIcon name="shield" />TLS · 验证证书</span>
              <span v-else-if="item.tls?.mode === 'require'" class="sec-chip warn"><ConnIcon name="lock" />TLS · 不验证证书</span>
              <span v-else class="sec-chip warn"><ConnIcon name="unlock" />未加密</span>
              <span v-if="item.ssh" class="sec-chip"><ConnIcon name="tunnel" />SSH · {{ item.ssh.host }}</span>
              <span class="sec-chip"><ConnIcon name="key" />{{ item.passwordSaved ? "密码在系统凭据库" : "每次询问密码" }}</span>
            </template>
          </div>
          <footer>
            <span class="conn-state" :class="`st-${statuses[item.id] ?? 'disconnected'}`"><i></i>{{ statuses[item.id] === "connected" ? "已连接" : statuses[item.id] === "connecting" ? "正在连接…" : statuses[item.id] === "error" ? "连接失败" : "未连接" }}</span>
            <span class="grow"></span>
            <button v-if="statuses[item.id] === 'connected'" class="btn sm" type="button" @click.stop="disconnect(item)"><ConnIcon name="unplug" />断开</button>
            <button v-else class="btn sm primary" type="button" data-action="conn-connect" @click.stop="requestConnect(item)"><ConnIcon name="plug" />连接</button>
            <button class="icon-btn" type="button" :aria-label="`${item.name} 的更多操作`" data-action="conn-menu" @click.stop="menuId = menuId === item.id ? null : item.id"><ConnIcon name="more" /></button>
          </footer>
          <div v-if="menuId === item.id" class="conn-menu" @click.stop>
            <button type="button" @click="showDetails(item)"><ConnIcon name="info" />连接详情</button>
            <button type="button" data-action="conn-edit" @click="openEditor(item)"><ConnIcon name="pencil" />编辑配置…</button>
            <button type="button" class="danger" data-action="conn-delete" @click="askDelete(item)"><ConnIcon name="trash" />删除配置…</button>
          </div>
        </article>
        <button class="conn-card add" type="button" data-action="conn-new" @click="openEditor()">
          <ConnIcon name="plus" /><span>新建连接</span><small>PostgreSQL · MySQL · MariaDB · SQLite</small>
        </button>
      </div>
      <p v-if="query && !filtered.length" class="muted">没有匹配“{{ query }}”的连接。</p>
    </div>

    <dialog ref="editDialog" class="modal modal-xl" @cancel.prevent="closeEditor">
      <div class="modal-head">
        <div>
          <h2>{{ draft.id ? `编辑连接 · ${draft.name || draft.host || draft.filePath}` : "新建连接" }}</h2>
          <p class="modal-sub">配置只保存在本机</p>
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
        <div class="ce-body" :class="{ single: draft.engine === 'sqlite' }">
          <nav v-if="draft.engine !== 'sqlite'" class="ce-nav" aria-label="配置分区">
            <button type="button" :class="{ on: section === 'general' }" @click="chooseSection('general')"><b>常规</b><small>{{ draft.host || "主机与认证" }}</small></button>
            <button type="button" :class="{ on: section === 'tls' }" data-section="tls" @click="chooseSection('tls')"><b>TLS / SSL</b><small>{{ draft.tlsMode === "verify" ? "验证证书" : draft.tlsMode === "require" ? "加密不验证" : "不加密" }}</small></button>
            <button type="button" :class="{ on: section === 'ssh' }" data-section="ssh" @click="chooseSection('ssh')"><b>SSH 隧道</b><small>{{ draft.sshOn ? draft.sshHost || "已启用" : "未启用" }}</small></button>
          </nav>
          <div class="ce-form">
            <template v-if="draft.engine === 'sqlite'">
              <div class="form-grid">
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
              <fieldset class="field env-field">
                <legend>环境标签</legend>
                <div class="env-pick">
                  <label v-for="item in ENVS" :key="item.id" :class="{ on: draft.env === item.id }">
                    <input type="radio" name="cf-env" :value="item.id" :checked="draft.env === item.id" @change="setEnv(item.id)" />
                    <span class="env" :class="`env-${item.id}`">{{ item.label }}</span>
                  </label>
                </div>
              </fieldset>
              <div class="callout callout-info"><ConnIcon name="info" /><div>SQLite 是本地文件，不需要 TLS 或 SSH。连接后会显示 <code>PRAGMA foreign_keys</code> 的实际状态。SQLite 默认不强制外键。</div></div>
            </template>
            <template v-else-if="section === 'general'">
              <div class="form-grid">
                <label class="field full"><span>连接名称</span><input v-model="draft.name" placeholder="例如 Atlas" /></label>
                <label class="field"><span>主机<em class="req">*</em></span><input v-model="draft.host" class="mono" data-field="host" placeholder="db.example.internal" :aria-invalid="Boolean(fieldErrors.host)" /><small v-if="fieldErrors.host" class="field-err">{{ fieldErrors.host }}</small></label>
                <label class="field"><span>端口<em class="req">*</em></span><input v-model="draft.port" class="mono" data-field="port" :aria-invalid="Boolean(fieldErrors.port)" /><small v-if="fieldErrors.port" class="field-err">{{ fieldErrors.port }}</small></label>
                <label class="field"><span>数据库</span><input v-model="draft.database" class="mono" :placeholder="draft.engine === 'postgres' ? 'postgres' : '可选'" /></label>
                <label class="field"><span>用户名<em class="req">*</em></span><input v-model="draft.username" class="mono" data-field="username" :aria-invalid="Boolean(fieldErrors.username)" /><small v-if="fieldErrors.username" class="field-err">{{ fieldErrors.username }}</small></label>
                <label class="field full"><span>密码</span><input v-model="secrets.password" type="password" autocomplete="new-password" /><small>留空则继续使用系统凭据库里已保存的密码。密码不会写入普通配置。</small></label>
                <label class="check full"><input v-model="draft.savePassword" type="checkbox" /><span>把密码保存到系统凭据库<small>不勾选时，每次连接都会询问密码。</small></span></label>
              </div>
              <fieldset class="field">
                <legend>环境标签</legend>
                <div class="env-pick">
                  <label v-for="item in ENVS" :key="item.id" :class="{ on: draft.env === item.id }">
                    <input type="radio" name="cf-env" :value="item.id" :checked="draft.env === item.id" @change="setEnv(item.id)" />
                    <span class="env" :class="`env-${item.id}`">{{ item.label }}</span>
                  </label>
                </div>
              </fieldset>
              <div class="route-box"><span class="muted sm">连接路径</span><div class="route"><ConnIcon name="monitor" /><span>本机</span><template v-if="draft.sshOn"><i class="route-line"></i><span class="route-hop"><ConnIcon name="tunnel" />{{ draft.sshHost || "跳板机" }}:{{ draft.sshPort }}</span></template><i class="route-line"></i><span class="route-hop"><ConnIcon name="database" />{{ draft.host || "数据库主机" }}:{{ draft.port || "?" }}</span></div></div>
            </template>
            <template v-else-if="section === 'tls'">
              <div class="radio-stack">
                <label v-for="item in ([['verify', '要求加密，并验证证书和主机名', '推荐。主机名不符时直接失败。'], ['require', '要求加密，但不验证证书', '握手失败时不会退回明文。'], ['off', '不加密', '只适合本机或完全可信的网络。']] as [TlsMode, string, string][])" :key="item[0]" class="radio-card" :class="{ on: draft.tlsMode === item[0] }">
                  <input type="radio" name="cf-tls" :value="item[0]" :checked="draft.tlsMode === item[0]" @change="draft.tlsMode = item[0]" />
                  <span><b>{{ item[1] }}</b><small>{{ item[2] }}</small></span>
                </label>
              </div>
              <div v-if="draft.tlsMode !== 'off'" class="form-grid">
                <label class="field"><span>受信任的 CA</span><select v-model="draft.caMode"><option value="system">系统信任根</option><option value="file">指定 CA 文件</option></select></label>
                <label v-if="draft.caMode === 'file'" class="field"><span>CA 证书文件</span><input v-model="draft.caFile" class="mono" /><small v-if="fieldErrors.caFile" class="field-err">{{ fieldErrors.caFile }}</small></label>
                <label class="field"><span>客户端证书（可选）</span><input v-model="draft.clientCert" class="mono" /></label>
                <label class="field"><span>客户端私钥（可选）</span><input v-model="draft.clientKey" class="mono" /></label>
              </div>
              <div v-if="draft.tlsMode === 'verify'" class="tls-name">
                <ConnIcon name="shield" />
                <div>
                  <b>证书校验使用的主机名：<code>{{ draft.host || "（填写主机后显示）" }}</code></b>
                  <small data-tls-copy>{{ tlsVerificationCopy(draft) }}</small>
                </div>
              </div>
              <div v-if="draft.tlsMode !== 'off'" class="callout callout-warn"><ConnIcon name="info" /><div>验证失败时连接直接终止：不会自动降低验证级别，也不会退回不加密连接重试。</div></div>
              <div v-else class="callout callout-warn"><ConnIcon name="unlock" /><div>连接不加密：数据库密码和查询数据会以明文在网络上传输。</div></div>
            </template>
            <template v-else>
              <label class="check"><input v-model="draft.sshOn" type="checkbox" data-field="sshOn" /><span><b>通过 SSH 跳板机连接</b><small>首版支持单跳。每条本地连接单独打开一条到数据库的通道。</small></span></label>
              <div v-if="draft.sshOn" class="form-grid">
                <label class="field"><span>跳板机主机<em class="req">*</em></span><input v-model="draft.sshHost" class="mono" data-field="sshHost" /><small v-if="fieldErrors.sshHost" class="field-err">{{ fieldErrors.sshHost }}</small></label>
                <label class="field"><span>SSH 端口</span><input v-model="draft.sshPort" class="mono" /><small v-if="fieldErrors.sshPort" class="field-err">{{ fieldErrors.sshPort }}</small></label>
                <label class="field"><span>SSH 用户<em class="req">*</em></span><input v-model="draft.sshUser" class="mono" data-field="sshUser" /><small v-if="fieldErrors.sshUser" class="field-err">{{ fieldErrors.sshUser }}</small></label>
                <label class="field"><span>认证方式</span><span class="seg-radio"><label :class="{ on: draft.sshAuth === 'key' }"><input type="radio" name="cf-sshauth" value="key" :checked="draft.sshAuth === 'key'" @change="draft.sshAuth = 'key'" />私钥</label><label :class="{ on: draft.sshAuth === 'password' }"><input type="radio" name="cf-sshauth" value="password" :checked="draft.sshAuth === 'password'" @change="draft.sshAuth = 'password'" />密码</label></span></label>
                <template v-if="draft.sshAuth === 'key'">
                  <label class="field"><span>私钥文件</span><input v-model="draft.sshKey" class="mono" /></label>
                  <label class="field"><span>私钥口令（如有）</span><input v-model="secrets.passphrase" type="password" autocomplete="new-password" /><small>只用于解锁私钥，不写入配置或日志。</small></label>
                </template>
                <label v-else class="field full"><span>SSH 密码</span><input v-model="secrets.sshPassword" type="password" autocomplete="new-password" /></label>
              </div>
              <div v-if="draft.sshOn" class="host-key">
                <ConnIcon name="key" />
                <div>
                  <b>主机密钥</b>
                  <small v-if="knownHostKey">已信任 <code>{{ knownHostKey }}</code>。如果服务器出示的密钥与此不同，连接会被阻止。</small>
                  <small v-else>尚未记录。首次连接时会显示指纹，需要你核对后确认。</small>
                </div>
                <button v-if="knownHostKey" class="btn sm ghost" type="button" @click="forgetHostKey">忘记已保存的密钥</button>
              </div>
              <div class="route-box"><span class="muted sm">连接路径</span><div class="route"><span>本机</span><template v-if="draft.sshOn"><i class="route-line"></i><span class="route-hop">{{ draft.sshHost || "跳板机" }}:{{ draft.sshPort }}</span></template><i class="route-line"></i><span class="route-hop">{{ draft.host || "数据库主机" }}:{{ draft.port || "?" }}</span></div></div>
            </template>
          </div>
        </div>
        <div v-if="report" class="test-panel" :class="report.status" role="status">
          <div class="test-head">
            <b>{{ report.status === "ok" ? "连接测试通过" : report.status === "wait" ? "等待你确认主机身份" : `连接测试失败：${report.failStage || ""}` }}</b>
            <span v-if="report.summary" class="muted sm">{{ report.summary }}</span>
          </div>
          <ol class="test-steps">
            <li v-for="stage in report.stages" :key="stage.label" class="ts" :class="`ts-${stage.status === 'skipped' ? 'skip' : stage.status}`">
              <span class="ts-i"><ConnIcon :name="stage.status === 'ok' ? 'check' : stage.status === 'err' ? 'x' : stage.status === 'wait' ? 'key' : 'minus'" /></span>
              <span class="ts-name">{{ stage.label }}</span>
              <span class="ts-msg">{{ stage.message }}</span>
            </li>
          </ol>
          <div v-if="report.status === 'wait'" class="fp-card">
            <p>首次连接 <code>{{ draft.sshHost }}:{{ draft.sshPort }}</code>。请通过可信渠道核对指纹，确认一致后再继续。</p>
            <pre>{{ report.algorithm }}  {{ report.fingerprint }}</pre>
            <div class="fp-actions">
              <button class="btn sm" type="button" @click="rejectHostKey">不信任，停止</button>
              <button class="btn sm primary" type="button" @click="trustHostKey">指纹一致，信任并继续</button>
            </div>
          </div>
          <p v-if="report.hint" class="test-hint">{{ report.hint }}</p>
        </div>
        <p v-else class="muted sm">测试阶段：{{ plannedStageLabels(draft).join(" → ") }}</p>
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
        <div class="modal-head"><div><h2>删除连接配置？</h2><p class="modal-sub">{{ deleteTarget.name }}</p></div></div>
        <div class="modal-body">
          <ul class="facts">
            <li>删除澄库中保存的这条配置，以及系统凭据库中对应的密码。</li>
            <li><b>不会</b>删除{{ deleteTarget.engine === "sqlite" ? ` SQLite 文件 ${deleteTarget.filePath}` : "数据库或其中的任何数据" }}。</li>
          </ul>
        </div>
        <div class="modal-foot"><span class="grow"></span><button class="btn" type="button" @click="closeDialog(otherDialog)">取消</button><button class="btn primary" type="button" data-action="confirm-delete" @click="confirmDelete">删除配置</button></div>
      </template>
      <template v-else-if="dialog === 'password' && passwordTarget">
        <div class="modal-head"><div><h2>连接 {{ passwordTarget.name }}</h2><p class="modal-sub">此连接没有保存密码，因此每次都会询问。</p></div></div>
        <div class="modal-body">
          <label class="field"><span>数据库密码</span><input v-model="promptPassword" type="password" autocomplete="current-password" /></label>
          <label class="check"><input v-model="promptSave" type="checkbox" /><span>保存到系统凭据库</span></label>
        </div>
        <div class="modal-foot"><span class="grow"></span><button class="btn" type="button" @click="closeDialog(otherDialog)">取消</button><button class="btn primary" type="button" @click="submitPrompt">连接</button></div>
      </template>
      <template v-else-if="detailTarget">
        <div class="modal-head"><div><h2>{{ detailTarget.name }}</h2><p class="modal-sub">{{ engineMeta(detailTarget.engine).label }}</p></div></div>
        <div class="modal-body">
          <dl class="kv">
            <dt>密码</dt><dd>{{ detailTarget.passwordSaved ? "保存在系统凭据库" : "每次询问" }}</dd>
            <dt>传输</dt><dd>{{ detailTarget.engine === "sqlite" ? "本地文件，没有 TLS" : detailTarget.tls?.mode === "verify" ? `按主机名 ${detailTarget.tls.serverName} 校验` : detailTarget.tls?.mode === "require" ? "已加密 · 未验证证书" : "未加密" }}</dd>
          </dl>
        </div>
        <div class="modal-foot"><button class="btn" type="button" @click="openEditor(detailTarget)">编辑配置</button><span class="grow"></span><button class="btn" type="button" @click="closeDialog(otherDialog)">关闭</button></div>
      </template>
    </dialog>
  </div>
</template>
