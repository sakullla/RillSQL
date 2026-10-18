<script setup lang="ts">
import { computed, nextTick, onMounted, onUpdated, ref, watch } from "vue";
import { tauriConnectionApi, type OpenOutcome } from "../connections/api";
import { ContextMenu, type MenuItem } from "../ui";
import { focusDatabase, listConnections, openConnection } from "./backend";
import { browseSql, qualifiedObjectName, type CatalogObjectRef } from "./object-ref";
import SchemaBlock from "./SchemaBlock.vue";
import { catalogConnections, catalogLoadError, refreshCatalog, schemaKey } from "./store";
import { engineShape, isConnected, objectPath, postgresSessionGap, searchObjects, splitMatch } from "./tree";
import type { CatalogConnection, DatabaseNode } from "./types";

type ConnectionMenuAction = "edit" | "test" | "copy" | "delete";

const props = defineProps<{ filter: string }>();
const emit = defineEmits<{
  "open-object": [object: CatalogObjectRef, pane: "data" | "structure", section?: "columns" | "drop"];
  "query-object": [object: CatalogObjectRef, sql: string];
  "focused-database": [connectionId: string, database: string];
  "connection-action": [connectionId: string, action: ConnectionMenuAction];
}>();
const menu = ref<{ object?: CatalogObjectRef; connection?: CatalogConnection; x: number; y: number } | null>(null);
const menuEl = ref<HTMLElement | null>(null);
const treeEl = ref<HTMLElement | null>(null);
const activeKey = ref<string | null>(null);
const refreshing = ref(false);
const focusing = ref<string | null>(null);
const focusTarget = ref<string | null>(null);
const focusError = ref("");
const connecting = ref<string | null>(null);
const connectTarget = ref<string | null>(null);
const connectError = ref("");
const passwordFor = ref<string | null>(null);
const typedPassword = ref("");

const ICONS = {
  database: "M4 6c0-1.7 3.6-3 8-3s8 1.3 8 3-3.6 3-8 3-8-1.3-8-3ZM4 6v12c0 1.7 3.6 3 8 3s8-1.3 8-3V6M4 12c0 1.7 3.6 3 8 3s8-1.3 8-3",
  schema: "m12 3 9 5-9 5-9-5 9-5ZM3 13l9 5 9-5",
  table: "M4 4h16a1 1 0 0 1 1 1v14a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V5a1 1 0 0 1 1-1ZM3 10h18M10 10v10",
  right: "m9 6 6 6-6 6",
  down: "m6 9 6 6 6-6",
  info: "M2 12a10 10 0 1 0 20 0 10 10 0 1 0-20 0M12 11v5.5M12 7.5h.01",
  error: "M2 12a10 10 0 1 0 20 0 10 10 0 1 0-20 0M15 9l-6 6M9 9l6 6",
  warn: "M12 3.5 2.5 20h19ZM12 10v4.5M12 17.5h.01",
} as const;

const ENGINE_SHORT = { postgres: "PG", mysql: "My", mariadb: "Ma", sqlite: "SQ" } as const;
const ENGINE_LABEL = { postgres: "PostgreSQL", mysql: "MySQL", mariadb: "MariaDB", sqlite: "SQLite" } as const;
const STATUS_TEXT = {
  connected: "已连接",
  disconnected: "未连接",
  connecting: "正在连接",
  error: "连接失败",
  lost: "连接已断开",
} as const;

const connections = computed(() => catalogConnections());
const loadError = computed(() => catalogLoadError());
const matches = computed(() => searchObjects(connections.value, props.filter));
const expanded = ref<Record<string, boolean>>({});

watch(
  connections,
  (list) => {
    const next = { ...expanded.value };
    for (const connection of list) {
      if (!isConnected(connection.status)) continue;
      const connectionKey = `c:${connection.id}`;
      if (next[connectionKey] === undefined) next[connectionKey] = true;
      const shape = engineShape(connection.engine);
      for (const database of connection.databases) {
        if (!shape.databases) continue;
        const databaseKey = `d:${connection.id}:${database.name}`;
        if (next[databaseKey] === undefined) next[databaseKey] = true;
        if (!shape.schemas) continue;
        for (const schema of database.schemas) {
          const key = schemaKey(connection.id, database.name, schema.name);
          if (next[key] === undefined) next[key] = true;
        }
      }
    }
    expanded.value = next;
  },
  { immediate: true },
);

function open(key: string): boolean {
  return expanded.value[key] === true;
}

function toggle(key: string) {
  expanded.value = { ...expanded.value, [key]: !open(key) };
}

function onConnectionRow(connection: CatalogConnection) {
  const key = `c:${connection.id}`;
  if (isConnected(connection.status)) {
    toggle(key);
    return;
  }
  expanded.value = { ...expanded.value, [key]: true };
  return connectSession(connection);
}

function depthStyle(depth: number) {
  return { "--d": String(depth) };
}

function sessionGap(database: DatabaseNode) {
  return postgresSessionGap(database.error);
}

function focusKey(connectionId: string, database: string) {
  return `${connectionId}:${database}`;
}

function failureText(error: unknown): string {
  if (typeof error === "string" && error) return error;
  if (error instanceof Error && error.message) return error.message;
  if (error && typeof error === "object" && "message" in error && typeof error.message === "string") return error.message;
  return "没有连上这个库，原来的会话还在。";
}

function openFailure(report: OpenOutcome["report"]): string {
  if (report.status === "wait") return report.hint || "需要先在连接页确认主机身份。";
  const stage = report.stages.find((item) => item.status === "err" && item.message);
  return stage?.message || report.hint || report.failStage || "没有连上。";
}

async function onRefresh() {
  if (refreshing.value) return;
  refreshing.value = true;
  try {
    await refreshCatalog();
  } finally {
    refreshing.value = false;
  }
}

async function connectSession(connection: CatalogConnection, password = "") {
  if (connecting.value) return;
  connecting.value = connection.id;
  connectTarget.value = connection.id;
  connectError.value = "";
  try {
    if (!password && connection.engine !== "sqlite") {
      const saved = (await listConnections()).find((item) => item.id === connection.id);
      if (saved && !saved.passwordSaved) {
        passwordFor.value = connection.id;
        typedPassword.value = "";
        return;
      }
    }
    const outcome = await openConnection(connection.id, password);
    if (!outcome.connected) {
      const message = openFailure(outcome.report);
      connectError.value = message;
      if (message.includes("请填写数据库密码")) passwordFor.value = connection.id;
      return;
    }
    passwordFor.value = null;
    typedPassword.value = "";
    await refreshCatalog();
  } catch (error) {
    connectError.value = failureText(error);
  } finally {
    connecting.value = null;
  }
}

async function focusSession(connection: CatalogConnection, database: string) {
  const key = focusKey(connection.id, database);
  if (focusing.value) return;
  focusing.value = key;
  focusTarget.value = key;
  focusError.value = "";
  try {
    await focusDatabase(connection.id, database);
    await refreshCatalog();
    emit("focused-database", connection.id, database);
  } catch (error) {
    focusError.value = failureText(error);
  } finally {
    focusing.value = null;
  }
}

function rowOf(target: EventTarget | null): HTMLElement | null {
  if (!(target instanceof Element)) return null;
  const row = target.closest("[data-level=object], [data-hit]");
  return row instanceof HTMLElement ? row : null;
}

function readRef(row: HTMLElement): CatalogObjectRef | null {
  const connectionId = row.dataset.connection || row.closest("[data-connection]")?.getAttribute("data-connection") || "";
  const database = row.dataset.database ?? "";
  const schema = row.dataset.schema ?? "";
  const name = row.dataset.object ?? "";
  if (!connectionId || !name) return null;
  const connection = catalogConnections().find((item) => item.id === connectionId);
  const object = connection?.databases.find((item) => item.name === database)?.schemas.find((item) => item.name === schema)?.objects.find((item) => item.name === name);
  if (!connection || !object) return null;
  return {
    connectionId,
    connectionName: connection.name,
    engine: connection.engine,
    database,
    schema,
    name: object.name,
    kind: object.kind,
    stale: object.stale,
    path: objectPath(connection.name, database, schema),
  };
}

function closeMenu() {
  menu.value = null;
}

function openObject(object: CatalogObjectRef | null, pane: "data" | "structure", section?: "columns" | "drop") {
  if (!object || object.stale) return;
  closeMenu();
  emit("open-object", object, pane, section);
}

function onTreeClick(event: MouseEvent) {
  const row = rowOf(event.target);
  if (!row) return;
  openObject(readRef(row), "data");
}

function onContextMenu(event: MouseEvent) {
  event.preventDefault();
  const target = event.target;
  if (!(target instanceof Element)) {
    closeMenu();
    return;
  }
  const row = target.closest<HTMLElement>(".tr-conn, [data-level=object], [data-hit]");
  if (!row) {
    closeMenu();
    return;
  }
  // 先把焦点放到触发行：菜单关闭时 ContextMenu 会把焦点还回来，键盘用户不掉焦点。
  row.focus();
  menuEl.value = row;
  if (row.classList.contains("tr-conn")) {
    const connectionId = row.closest("[data-connection]")?.getAttribute("data-connection") || "";
    const connection = connections.value.find((item) => item.id === connectionId);
    if (!connection) {
      closeMenu();
      return;
    }
    menu.value = { connection, x: Math.max(8, event.clientX), y: Math.max(8, event.clientY) };
    return;
  }
  const object = readRef(row);
  if (!object) {
    closeMenu();
    return;
  }
  menu.value = { object, x: Math.max(8, event.clientX), y: Math.max(8, event.clientY) };
}

const menuLabel = computed(() => {
  const current = menu.value;
  if (!current) return "操作";
  if (current.connection) return `${current.connection.name} 的操作`;
  return `${current.object?.name ?? ""} 的操作`;
});

const menuItems = computed<MenuItem[]>(() => {
  const current = menu.value;
  if (!current) return [];
  if (current.connection) {
    return [
      { kind: "item", id: "conn-toggle", label: isConnected(current.connection.status) ? "断开连接" : "连接" },
      { kind: "item", id: "conn-edit", label: "编辑…" },
      { kind: "item", id: "conn-test", label: "测试连接" },
      { kind: "item", id: "conn-copy", label: "复制配置" },
      { kind: "sep" },
      { kind: "item", id: "conn-delete", label: "删除…", danger: true },
    ];
  }
  const object = current.object;
  if (!object) return [];
  const items: MenuItem[] = [];
  if (object.stale) items.push({ kind: "head", text: "这个对象已失效——它在外部被删除，不能再打开" });
  items.push(
    { kind: "item", id: "open-data", label: "打开数据", disabled: object.stale },
    { kind: "item", id: "open-structure", label: "查看结构", disabled: object.stale },
    { kind: "item", id: "new-query", label: "新建查询到此对象", title: browseSql(object.engine, object.schema, object.name), disabled: object.stale },
    { kind: "item", id: "copy-name", label: "复制限定名" },
    { kind: "item", id: "gen-select", label: "生成 SELECT" },
  );
  if (object.kind === "table") {
    items.push({ kind: "sep" }, { kind: "item", id: "delete-item", label: "删除表…", danger: true, disabled: object.stale, title: "打开结构页的删除确认区" });
  }
  return items;
});

function onMenuSelect(id: string) {
  const current = menu.value;
  if (!current) return;
  if (current.object) {
    onObjectMenu(id, current.object);
    return;
  }
  if (current.connection) void onConnectionMenu(id, current.connection);
}

function onObjectMenu(id: string, object: CatalogObjectRef) {
  if (id === "open-data") return openObject(object, "data");
  if (id === "open-structure") return openObject(object, "structure", "columns");
  if (id === "new-query") {
    if (object.stale) return;
    emit("query-object", object, browseSql(object.engine, object.schema, object.name));
    return;
  }
  if (id === "copy-name") {
    void navigator.clipboard.writeText(qualifiedObjectName(object.engine, object.schema, object.name)).catch(() => undefined);
    return;
  }
  if (id === "gen-select") {
    void navigator.clipboard.writeText(browseSql(object.engine, object.schema, object.name)).catch(() => undefined);
    return;
  }
  if (id === "delete-item") return openObject(object, "structure", "drop");
}

async function onConnectionMenu(id: string, connection: CatalogConnection) {
  if (id === "conn-toggle") {
    if (isConnected(connection.status)) await disconnectSession(connection);
    else await connectSession(connection);
    return;
  }
  if (id === "conn-copy") {
    await copyConnectionConfig(connection);
    return;
  }
  if (id === "conn-edit" || id === "conn-test" || id === "conn-delete") {
    emit("connection-action", connection.id, id.slice("conn-".length) as ConnectionMenuAction);
  }
}

async function disconnectSession(connection: CatalogConnection) {
  try {
    await tauriConnectionApi.disconnect(connection.id);
  } catch (error) {
    connectTarget.value = connection.id;
    connectError.value = failureText(error);
    return;
  }
  await refreshCatalog();
}

/** 复制不含密码、SSH 密码、私钥口令和主机密钥的配置 JSON。 */
async function copyConnectionConfig(connection: CatalogConnection) {
  const saved = (await listConnections().catch(() => [] as const)).find((item) => item.id === connection.id);
  const payload = saved
    ? {
        id: saved.id,
        name: saved.name,
        engine: saved.engine,
        env: saved.env,
        host: saved.host,
        port: saved.port,
        database: saved.database,
        username: saved.username,
        filePath: saved.filePath,
        tls: saved.tls,
        ssh: saved.ssh,
      }
    : { id: connection.id, name: connection.name, engine: connection.engine };
  await navigator.clipboard.writeText(JSON.stringify(payload, null, 2)).catch(() => undefined);
}

/** 键盘导航：行不带 tabindex 时 focus() 无效，SchemaBlock 的行也要补上（roving tabindex）。 */
function rowKeyOf(row: HTMLElement): string {
  const connectionId = row.dataset.connection || row.closest("[data-connection]")?.getAttribute("data-connection") || "";
  if (row.classList.contains("tr-conn")) return `c:${connectionId}`;
  if (row.hasAttribute("data-hit")) {
    return `h:${connectionId}:${row.dataset.database ?? ""}:${row.dataset.schema ?? ""}:${row.dataset.object ?? ""}`;
  }
  if (row.dataset.level === "database") return `d:${connectionId}:${row.dataset.database ?? ""}`;
  if (row.dataset.level === "schema") return schemaKey(connectionId, row.dataset.database ?? "", row.dataset.schema ?? "");
  return `o:${connectionId}:${row.dataset.database ?? ""}:${row.dataset.schema ?? ""}:${row.dataset.object ?? ""}`;
}

function treeRows(): HTMLElement[] {
  if (!treeEl.value) return [];
  return [...treeEl.value.querySelectorAll<HTMLElement>('.tree-row[role="treeitem"]')];
}

function stampRows() {
  const rows = treeRows();
  if (!rows.length) return;
  if (!rows.some((row) => rowKeyOf(row) === activeKey.value)) {
    activeKey.value = rowKeyOf(rows[0]!);
  }
  for (const row of rows) {
    row.tabIndex = rowKeyOf(row) === activeKey.value ? 0 : -1;
  }
}

function focusRow(row: HTMLElement) {
  activeKey.value = rowKeyOf(row);
  row.focus();
  stampRows();
}

function onTreeKey(event: KeyboardEvent) {
  const rows = treeRows();
  if (!rows.length) return;
  const active = document.activeElement instanceof HTMLElement && rows.includes(document.activeElement) ? document.activeElement : null;
  if (event.key === "ArrowDown" || event.key === "ArrowUp") {
    event.preventDefault();
    const at = active ? rows.indexOf(active) : -1;
    const next = event.key === "ArrowDown" ? (at < 0 ? 0 : Math.min(at + 1, rows.length - 1)) : at < 0 ? rows.length - 1 : Math.max(at - 1, 0);
    focusRow(rows[next]!);
    return;
  }
  if (event.key === "Enter") {
    if (!active) return;
    event.preventDefault();
    active.click();
    return;
  }
  if (event.key === "F4") {
    if (!active || !active.matches("[data-level=object], [data-hit]")) return;
    event.preventDefault();
    const object = readRef(active);
    if (object) emit("open-object", object, "structure", "columns");
  }
}

onMounted(() => stampRows());
onUpdated(() => stampRows());
watch([expanded, connections, () => props.filter], () => {
  void nextTick(() => stampRows());
});
</script>

<template>
  <div ref="treeEl" class="tree" role="tree" aria-label="数据库对象" @click="onTreeClick" @contextmenu="onContextMenu" @keydown="onTreeKey" @scroll="closeMenu">
    <div class="tree-toolbar">
      <button class="btn ghost sm" type="button" data-action="tree-refresh" aria-label="刷新对象列表" :disabled="refreshing" @click="onRefresh">
        {{ refreshing ? "正在刷新…" : "刷新" }}
      </button>
    </div>
    <div v-if="loadError" class="tree-note err" data-load-error>
      <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="ICONS.error" /></svg>
      <div>
        <strong>对象列表没读出来</strong>
        <span class="reason">{{ loadError }}</span>
        <span class="tree-note-acts">
          <button class="btn sm" type="button" data-action="tree-retry" @click="onRefresh">重试</button>
        </span>
      </div>
    </div>
    <template v-if="filter.trim()">
      <div class="tree-filter-head">找到 {{ matches.hits.length }} 个名称包含“{{ filter.trim() }}”的对象</div>
      <div
        v-for="hit in matches.hits"
        :key="hit.id"
        class="tree-row tr-hit"
        :class="{ dropped: hit.stale }"
        role="treeitem"
        data-hit
        :data-connection="hit.connectionId"
        :data-database="hit.database"
        :data-schema="hit.schema"
        :data-path="hit.path"
        :data-object="hit.name"
        :data-kind="hit.kind"
        :data-stale="hit.stale ? 'true' : 'false'"
      >
        <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="ICONS.table" /></svg>
        <span class="hit-text">
          <span class="tr-name">
            <template v-for="(part, index) in splitMatch(hit.name, filter)" :key="index">
              <mark v-if="part.hit">{{ part.text }}</mark>
              <template v-else>{{ part.text }}</template>
            </template>
          </span>
          <span class="hit-path">
            <span v-if="hit.env !== 'none' && hit.envTone !== 'none'" class="env xs" :class="`env-${hit.envTone}`">{{ hit.envLabel }}</span>
            {{ hit.path }}
            <span v-if="hit.stale" class="badge err" title="这个对象已在外部被删除，下次刷新后不再显示">已失效</span>
          </span>
        </span>
      </div>
      <div v-if="matches.hits.length === 0" class="tree-note" style="--d: 0">
        <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="ICONS.info" /></svg>
        <span>已连接的数据库里没有名称包含“{{ filter.trim() }}”的表或视图</span>
      </div>
      <div v-if="matches.skipped.length" class="tree-note subtle" style="--d: 0" data-skipped>
        <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="ICONS.info" /></svg>
        <span>{{ matches.skipped.join("、") }} 未连接，没有参与筛选</span>
      </div>
    </template>
    <p v-else-if="connections.length === 0 && !loadError" class="tree-note">还没有打开的连接。到「连接」页新建连接后，这里会列出数据库、schema 和表。</p>
    <template v-else>
      <section v-for="connection in connections" :key="connection.id" :data-engine="connection.engine" :data-connection="connection.id">
        <div
          class="tree-row tr-conn"
          :class="`st-${connection.status}`"
          role="treeitem"
          :style="depthStyle(0)"
          :aria-expanded="open(`c:${connection.id}`)"
          :title="`${connection.name} · ${ENGINE_LABEL[connection.engine]} · ${STATUS_TEXT[connection.status]}`"
          @click="onConnectionRow(connection)"
        >
          <span class="tw" @click.stop="toggle(`c:${connection.id}`)">
            <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="open(`c:${connection.id}`) ? ICONS.down : ICONS.right" /></svg>
          </span>
          <span class="eng" :class="`eng-${connection.engine}`">{{ ENGINE_SHORT[connection.engine] }}</span>
          <span class="tr-name">{{ connection.name }}</span>
          <span v-if="connection.env !== 'none' && connection.envTone !== 'none'" class="env xs" :class="`env-${connection.envTone}`">{{ connection.envLabel }}</span>
          <span class="conn-acts">
            <button
              v-if="!isConnected(connection.status) && passwordFor !== connection.id"
              class="btn sm"
              type="button"
              data-action="tree-connect"
              :data-connection="connection.id"
              :disabled="connecting === connection.id || connection.status === 'connecting'"
              @click.stop="connectSession(connection)"
            >
              {{ connecting === connection.id ? "正在连接…" : connection.status === "lost" ? "重新连接" : "连接" }}
            </button>
            <span class="conn-state" :class="`st-${connection.status}`">
              <i></i><span v-if="connection.status !== 'connected'">{{ STATUS_TEXT[connection.status] }}</span>
            </span>
          </span>
        </div>
        <template v-if="open(`c:${connection.id}`)">
          <div v-if="!isConnected(connection.status)" class="tree-note" :class="{ warn: connection.status === 'lost' }" :style="depthStyle(1)" data-disconnected>
            <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="connection.status === 'lost' ? ICONS.warn : ICONS.info" /></svg>
            <div>
              <span>{{ connection.status === "lost" ? "连接已断开，这里显示的是断开前读到的内容，可能不是最新。" : "未连接——连接后才会读取这里的表。" }}</span>
              <form v-if="passwordFor === connection.id" class="tree-connect" @submit.prevent="connectSession(connection, typedPassword)">
                <input v-model="typedPassword" type="password" autocomplete="current-password" aria-label="数据库密码" />
                <button class="btn sm primary" type="submit" data-action="tree-connect" :data-connection="connection.id" :disabled="connecting === connection.id">连接</button>
              </form>
              <span v-if="connectError && connectTarget === connection.id" class="tree-inline-err" data-connect-error>{{ connectError }}</span>
            </div>
          </div>
          <template v-else>
          <div v-if="connection.error" class="tree-note err" data-connection-error :style="depthStyle(1)">
            <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="ICONS.error" /></svg>
            <div>
              <strong>对象列表没读出来</strong>
              <span class="reason">{{ connection.error }}</span>
              <span class="tree-note-acts">
                <button class="btn sm" type="button" data-action="tree-retry" @click="onRefresh">重试</button>
              </span>
            </div>
          </div>
          <template v-if="!engineShape(connection.engine).databases">
            <div v-if="connection.foreignKeys !== null" class="tree-note subtle" :style="depthStyle(1)" data-foreign-keys>
              <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="ICONS.info" /></svg>
              <span>SQLite 的外键约束：{{ connection.foreignKeys ? "已开启" : "已关闭" }}（foreign_keys = {{ connection.foreignKeys ? "ON" : "OFF" }}）</span>
            </div>
            <template v-for="database in connection.databases" :key="database.name">
              <SchemaBlock
                v-for="schema in database.schemas"
                :key="`${database.name}:${schema.name}`"
                :connection="connection"
                :database="database.name"
                :schema="schema"
                :depth="1"
              />
            </template>
          </template>
          <template v-else>
            <template v-for="database in connection.databases" :key="database.name">
              <div
                class="tree-row"
                role="treeitem"
                data-level="database"
                :data-database="database.name"
                :style="depthStyle(1)"
                :aria-expanded="open(`d:${connection.id}:${database.name}`)"
                @click="toggle(`d:${connection.id}:${database.name}`)"
              >
                <span class="tw">
                  <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="open(`d:${connection.id}:${database.name}`) ? ICONS.down : ICONS.right" /></svg>
                </span>
                <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="ICONS.database" /></svg>
                <span class="tr-name">{{ database.name }}</span>
                <span v-if="database.name === connection.defaultDatabase" class="badge current-db" title="当前会话连接的数据库">当前</span>
              </div>
              <template v-if="open(`d:${connection.id}:${database.name}`)">
                <div v-if="sessionGap(database)" class="tree-note" :style="depthStyle(2)" data-other-database>
                  <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="ICONS.info" /></svg>
                  <div>
                    <span>这条会话现在连在 {{ sessionGap(database)?.current }}，PostgreSQL 一次只能读取一个库。要浏览 {{ database.name }} 的表，先把会话连过去。</span>
                    <span class="tree-note-acts">
                      <button
                        class="btn sm"
                        type="button"
                        data-action="focus-database"
                        :data-database="database.name"
                        :data-connection="connection.id"
                        :disabled="focusing === focusKey(connection.id, database.name)"
                        :title="`把当前会话改连到 ${database.name}。正在执行的语句会断开，已保存的连接配置不变。`"
                        @click.stop="focusSession(connection, database.name)"
                      >
                        {{ focusing === focusKey(connection.id, database.name) ? "正在连接…" : `连接到 ${database.name}` }}
                      </button>
                    </span>
                    <span v-if="focusError && focusTarget === focusKey(connection.id, database.name)" class="tree-inline-err" data-focus-error>{{ focusError }}</span>
                  </div>
                </div>
                <div v-else-if="database.error" class="tree-note err" :style="depthStyle(2)">
                  <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="ICONS.error" /></svg>
                  <div>
                    <strong>对象列表没读出来</strong>
                    <span class="reason">{{ database.error }}</span>
                    <span class="tree-note-acts">
                      <button class="btn sm" type="button" data-action="tree-retry" @click="onRefresh">重试</button>
                    </span>
                  </div>
                </div>
                <template v-for="schema in database.schemas" :key="`${database.name}:${schema.name}`">
                  <div
                    v-if="engineShape(connection.engine).schemas && schema.name"
                    class="tree-row"
                    role="treeitem"
                    data-level="schema"
                    :data-schema="schema.name"
                    :data-database="database.name"
                    :style="depthStyle(2)"
                    :aria-expanded="open(schemaKey(connection.id, database.name, schema.name))"
                    @click="toggle(schemaKey(connection.id, database.name, schema.name))"
                  >
                    <span class="tw">
                      <svg class="i" viewBox="0 0 24 24" aria-hidden="true">
                        <path :d="open(schemaKey(connection.id, database.name, schema.name)) ? ICONS.down : ICONS.right" />
                      </svg>
                    </span>
                    <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="ICONS.schema" /></svg>
                    <span class="tr-name">{{ schema.name }}</span>
                    <span v-if="!schema.error" class="tr-meta">{{ schema.total }}</span>
                  </div>
                  <SchemaBlock
                    v-if="!engineShape(connection.engine).schemas || open(schemaKey(connection.id, database.name, schema.name))"
                    :connection="connection"
                    :database="database.name"
                    :schema="schema"
                    :depth="engineShape(connection.engine).schemas ? 3 : 2"
                  />
                </template>
              </template>
            </template>
          </template>
          </template>
        </template>
      </section>
    </template>
  </div>
  <ContextMenu
    :open="Boolean(menu)"
    :x="menu?.x ?? 0"
    :y="menu?.y ?? 0"
    :items="menuItems"
    :label="menuLabel"
    :trigger="menuEl"
    @close="closeMenu"
    @select="onMenuSelect"
  />
</template>

<style src="./catalog.css"></style>
