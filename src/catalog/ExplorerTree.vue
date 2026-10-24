<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import type { OpenOutcome } from "../connections/api";
import { fetchDefinition, focusDatabase, listConnections, openConnection } from "./backend";
import { browseSql, qualifiedObjectName, type CatalogObjectRef } from "./object-ref";
import type { DiagramOpen } from "../diagram/types";
import SchemaBlock from "./SchemaBlock.vue";
import { catalogConnections, catalogLoadError, refreshCatalog, schemaKey, setStructureFocus } from "./store";
import { engineShape, isConnected, isDefinitionKind, KIND_LABEL, objectKey, objectPath, postgresSessionGap, searchObjects, splitMatch } from "./tree";
import type { CatalogConnection, DatabaseNode } from "./types";

const props = defineProps<{ filter: string }>();
const emit = defineEmits<{
  "open-object": [object: CatalogObjectRef, pane: "data" | "structure", section?: "columns" | "drop"];
  "open-definition": [object: CatalogObjectRef];
  "query-object": [object: CatalogObjectRef, sql: string];
  "focused-database": [connectionId: string, database: string];
  "open-diagram": [target: DiagramOpen];
  "new-connection": [];
}>();
const menu = ref<{ object: CatalogObjectRef; x: number; y: number; notice: string } | null>(null);
const selectedKey = ref<string | null>(null);
const refreshing = ref(false);
const focusing = ref<string | null>(null);
const focusTarget = ref<string | null>(null);
const focusError = ref("");
const connecting = ref<string | null>(null);
const connectTarget = ref<string | null>(null);
const connectError = ref("");
const passwordFor = ref<string | null>(null);
const typedPassword = ref("");
const structureNotice = ref("");

const ICONS = {
  database: "M4 6c0-1.7 3.6-3 8-3s8 1.3 8 3-3.6 3-8 3-8-1.3-8-3ZM4 6v12c0 1.7 3.6 3 8 3s8-1.3 8-3V6M4 12c0 1.7 3.6 3 8 3s8-1.3 8-3",
  schema: "m12 3 9 5-9 5-9-5 9-5ZM3 13l9 5 9-5",
  table: "M4 4h16a1 1 0 0 1 1 1v14a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V5a1 1 0 0 1 1-1ZM3 10h18M10 10v10",
  view: "M2 12s3.6-7 10-7 10 7 10 7-3.6 7-10 7S2 12 2 12ZM9 12a3 3 0 1 0 6 0 3 3 0 1 0-6 0",
  function: "M8 7h3M8 12h3M8 17h3M14 7l2 5-2 5M4 4h16v16H4z",
  procedure: "M6 7h12M6 12h12M6 17h8M4 4h16v16H4z",
  trigger: "M12 3v4M12 17v4M4.9 6.5l3.5 2M15.6 15.5l3.5 2M4.9 17.5l3.5-2M15.6 8.5l3.5-2",
  sequence: "M4 7h16M4 12h10M4 17h16",
  enum: "M5 5h6v6H5zM13 5h6v6h-6zM5 13h6v6H5zM13 13h6v6h-6z",
  domain: "M4 6h16v12H4zM8 10h8M8 14h5",
  right: "m9 6 6 6-6 6",
  down: "m6 9 6 6 6-6",
  info: "M2 12a10 10 0 1 0 20 0 10 10 0 1 0-20 0M12 11v5.5M12 7.5h.01",
  error: "M2 12a10 10 0 1 0 20 0 10 10 0 1 0-20 0M15 9l-6 6M9 9l6 6",
  warn: "M12 3.5 2.5 20h19ZM12 10v4.5M12 17.5h.01",
  refresh: "M20 12a8 8 0 1 1-2.4-5.7M20 3.5V8h-4.5",
  diagram: "M4 5h6v5H4zM14 5h6v5h-6zM9 16h6v5H9zM7 10v3h10v-3M12 13v3",
  structure: "M4 4h16v16H4zM4 10h16M10 10v10",
  query: "M6 4h12v16H6zM9 8h6M9 12h6M9 16h3",
  braces: "M8 6c-2 0-2 3-2 3s0 3-2 3c2 0 2 3 2 3s0 3 2 3M16 6c2 0 2 3 2 3s0 3 2 3c-2 0-2 3-2 3s0 3 2 3",
  copy: "M9 9h11v11H9zM4 15V4h11",
  tag: "M4 4h7l9 9-7 7-9-9zM8 8h.01",
  trash: "M4 7h16M9 7V4h6v3M6 7l1 13h10l1-13M10 11v6M14 11v6",
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

function openDiagram(connection: CatalogConnection, database: string, schema: string) {
  const path = [connection.name, database, schema].filter((part) => part.length > 0).join(" / ");
  emit("open-diagram", {
    connectionId: connection.id,
    connectionName: connection.name,
    database,
    engine: connection.engine,
    schema,
    path,
    label: schema ? `${schema} 关系图` : `${database || connection.name} 关系图`,
  });
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
  const row = target.closest("[data-level=member], [data-level=object], [data-hit]");
  return row instanceof HTMLElement ? row : null;
}

function readRef(row: HTMLElement): CatalogObjectRef | null {
  const connectionId = row.dataset.connection || row.closest("[data-connection]")?.getAttribute("data-connection") || "";
  const database = row.dataset.database ?? "";
  const schema = row.dataset.schema ?? "";
  const name = row.dataset.object ?? "";
  const key = row.dataset.key ?? "";
  if (!connectionId || !name) return null;
  const connection = catalogConnections().find((item) => item.id === connectionId);
  const objects = connection?.databases.find((item) => item.name === database)?.schemas.find((item) => item.name === schema)?.objects;
  const object = objects?.find((item) => (key ? objectKey(item) === key : item.name === name));
  if (!connection || !object) return null;
  return {
    connectionId,
    connectionName: connection.name,
    engine: connection.engine,
    database,
    schema,
    name: object.name,
    kind: object.kind,
    signature: object.signature,
    relation: object.relation,
    stale: object.stale,
    path: objectPath(connection.name, database, schema),
  };
}

function isRoutine(object: CatalogObjectRef): boolean {
  return isDefinitionKind(object.kind);
}

function memberSelection(row: HTMLElement): string {
  return `member:${row.dataset.key ?? ""}:${row.dataset.section ?? "columns"}:${row.dataset.member ?? ""}`;
}

function closeMenu() {
  menu.value = null;
}

function openObject(object: CatalogObjectRef | null, pane: "data" | "structure", section?: "columns" | "drop") {
  if (!object || object.stale) {
    if (object?.stale && pane === "structure") structureNotice.value = `${object.name} 已失效，不能打开结构。`;
    return;
  }
  if (pane === "structure") setStructureFocus(null);
  closeMenu();
  emit("open-object", object, pane, section);
}

function onTreeClick(event: MouseEvent) {
  const row = rowOf(event.target);
  if (!row) return;
  if (row.dataset.level === "member") {
    selectedKey.value = memberSelection(row);
    return;
  }
  const object = readRef(row);
  if (!object) return;
  selectedKey.value = objectKey(object);
}

/** 树行可用键盘到达：Enter 打开，空格选择/展开，Shift+F10 打开对象菜单。 */
function onTreeKeydown(event: KeyboardEvent) {
  const target = event.target;
  if (!(target instanceof Element) || target.closest("button")) return;
  const row = target.closest<HTMLElement>(".tree-row");
  if (!row) return;
  if (event.key === "Enter") {
    event.preventDefault();
    row.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    row.dispatchEvent(new MouseEvent("dblclick", { bubbles: true }));
    return;
  }
  if (event.key === " ") {
    event.preventDefault();
    row.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    return;
  }
  if (event.key === "ContextMenu" || (event.shiftKey && event.key === "F10")) {
    event.preventDefault();
    const rect = row.getBoundingClientRect();
    onContextMenu({
      target: row,
      clientX: rect.left + 24,
      clientY: rect.bottom,
      preventDefault: () => undefined,
    } as unknown as MouseEvent);
  }
}

function onTreeDblClick(event: MouseEvent) {
  const row = rowOf(event.target);
  if (!row) return;
  const object = readRef(row);
  if (!object || object.stale) {
    if (object?.stale) structureNotice.value = `${object.name} 已失效，不能打开结构。`;
    return;
  }
  if (row.dataset.level === "member") {
    const section = row.dataset.section === "indexes" ? "indexes" : "columns";
    const name = row.dataset.member ?? "";
    if (!name) return;
    selectedKey.value = memberSelection(row);
    structureNotice.value = "";
    setStructureFocus({
      connectionId: object.connectionId,
      database: object.database,
      schema: object.schema,
      table: object.name,
      section,
      name,
    });
    emit("open-object", object, "structure", "columns");
    return;
  }
  selectedKey.value = objectKey(object);
  if (isRoutine(object)) {
    emit("open-definition", object);
    return;
  }
  openObject(object, "data");
}

function onContextMenu(event: MouseEvent) {
  event.preventDefault();
  const row = rowOf(event.target);
  const object = row ? readRef(row) : null;
  if (!object) {
    closeMenu();
    return;
  }
  const height = object.kind === "table" ? 210 : isRoutine(object) ? 150 : 160;
  menu.value = {
    object,
    x: Math.max(8, Math.min(event.clientX, window.innerWidth - 228)),
    y: Math.max(8, Math.min(event.clientY, window.innerHeight - height)),
    notice: "",
  };
}

function onQuery() {
  const object = menu.value?.object;
  if (!object || object.stale) return;
  const sql = browseSql(object.engine, object.schema, object.name);
  closeMenu();
  emit("query-object", object, sql);
}

function onDefine() {
  const object = menu.value?.object;
  if (!object || object.stale || !isRoutine(object)) return;
  closeMenu();
  emit("open-definition", object);
}

async function onCopyDefinition() {
  const object = menu.value?.object;
  if (!object || object.stale || !isRoutine(object)) return;
  try {
    const sql = await fetchDefinition(object);
    closeMenu();
    emit("query-object", object, sql);
  } catch (error) {
    if (menu.value) menu.value = { ...menu.value, notice: failureText(error) };
  }
}

async function onCopy() {
  const object = menu.value?.object;
  if (!object) return;
  try {
    await navigator.clipboard.writeText(qualifiedObjectName(object.engine, object.schema, object.name));
    closeMenu();
  } catch {
    if (menu.value) menu.value = { ...menu.value, notice: "无法写入剪贴板" };
  }
}

function onWindowPointer(event: MouseEvent) {
  if (event.button === 2) return;
  const target = event.target;
  if (target instanceof Element && target.closest("[data-menu=object]")) return;
  closeMenu();
}

function onWindowKey(event: KeyboardEvent) {
  if (event.key === "Escape") closeMenu();
}

onMounted(() => {
  window.addEventListener("mousedown", onWindowPointer);
  window.addEventListener("keydown", onWindowKey);
});

onBeforeUnmount(() => {
  window.removeEventListener("mousedown", onWindowPointer);
  window.removeEventListener("keydown", onWindowKey);
});
</script>

<template>
  <div class="tree" role="tree" aria-label="数据库对象" @click="onTreeClick" @dblclick="onTreeDblClick" @contextmenu="onContextMenu" @keydown="onTreeKeydown" @scroll="closeMenu">
    <div class="tree-toolbar">
      <button class="btn ghost sm" type="button" data-action="tree-refresh" aria-label="刷新对象列表" :disabled="refreshing" @click="onRefresh">
        <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="ICONS.refresh" /></svg>
        {{ refreshing ? "正在刷新…" : "刷新" }}
      </button>
    </div>
    <div v-if="structureNotice" class="tree-note err" data-structure-notice>
      <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="ICONS.error" /></svg>
      <span>{{ structureNotice }}</span>
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
        role="treeitem"
        tabindex="0"
        data-hit
        :data-connection="hit.connectionId"
        :data-database="hit.database"
        :data-schema="hit.schema"
        :data-path="hit.path"
        :data-object="hit.name"
        :data-key="hit.key"
        :data-kind="hit.kind"
        :class="{ dropped: hit.stale, active: selectedKey === hit.key }"
        :data-stale="hit.stale ? 'true' : 'false'"
      >
        <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="ICONS[hit.kind]" /></svg>
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
    <div v-else-if="connections.length === 0 && !loadError" class="tree-note tree-empty">
      <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="ICONS.info" /></svg>
      <span>还没有打开的连接。新建连接后，这里会列出数据库、schema 和表。</span>
      <button class="btn sm" type="button" data-action="tree-new-connection" @click="emit('new-connection')">新建连接</button>
    </div>
    <template v-else>
      <section v-for="connection in connections" :key="connection.id" :data-engine="connection.engine" :data-connection="connection.id">
        <div
          class="tree-row tr-conn"
          :class="`st-${connection.status}`"
          role="treeitem"
          tabindex="0"
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
              <button
                class="tree-row"
                type="button"
                data-action="open-diagram"
                data-level="diagram"
                :data-connection="connection.id"
                :data-database="database.name"
                data-schema=""
                :style="depthStyle(1)"
                @click.stop="openDiagram(connection, database.name, '')"
              >
                <span class="tw"></span>
                <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="ICONS.diagram" /></svg>
                <span class="tr-name">关系图</span>
              </button>
              <SchemaBlock
                v-for="schema in database.schemas"
                :key="`${database.name}:${schema.name}`"
                :connection="connection"
                :database="database.name"
                :schema="schema"
                :depth="1"
                :selected-key="selectedKey ?? undefined"
              />
            </template>
          </template>
          <template v-else>
            <template v-for="database in connection.databases" :key="database.name">
              <div
                class="tree-row"
                role="treeitem"
                tabindex="0"
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
                <button
                  v-if="!engineShape(connection.engine).schemas && !sessionGap(database)"
                  class="tree-row"
                  type="button"
                  data-action="open-diagram"
                  data-level="diagram"
                  :data-connection="connection.id"
                  :data-database="database.name"
                  data-schema=""
                  :style="depthStyle(2)"
                  @click.stop="openDiagram(connection, database.name, '')"
                >
                  <span class="tw"></span>
                  <span class="tr-name">关系图</span>
                </button>
                <template v-for="schema in database.schemas" :key="`${database.name}:${schema.name}`">
                  <div
                    v-if="engineShape(connection.engine).schemas && schema.name"
                    class="tree-row"
                    role="treeitem"
                    tabindex="0"
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
                  <button
                    v-if="engineShape(connection.engine).schemas && schema.name && !sessionGap(database)"
                    class="tree-row"
                    type="button"
                    data-action="open-diagram"
                    data-level="diagram"
                    :data-connection="connection.id"
                    :data-database="database.name"
                    :data-schema="schema.name"
                    :style="depthStyle(3)"
                    @click.stop="openDiagram(connection, database.name, schema.name)"
                  >
                    <span class="tw"></span>
                    <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="ICONS.diagram" /></svg>
                    <span class="tr-name">关系图</span>
                  </button>
                  <SchemaBlock
                    v-if="!engineShape(connection.engine).schemas || open(schemaKey(connection.id, database.name, schema.name))"
                    :connection="connection"
                    :database="database.name"
                    :schema="schema"
                    :depth="engineShape(connection.engine).schemas ? 3 : 2"
                    :selected-key="selectedKey ?? undefined"
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
  <Teleport to="body">
    <div
      v-if="menu"
      class="menu object-menu"
      data-menu="object"
      role="menu"
      :aria-label="`${menu.object.name} 的操作`"
      :style="{ left: `${menu.x}px`, top: `${menu.y}px` }"
      @click.stop
      @contextmenu.prevent
    >
      <div class="menu-head">{{ menu.object.name }} · {{ KIND_LABEL[menu.object.kind] }}</div>
      <div v-if="menu.object.stale" class="menu-head">这个对象已失效——它在外部被删除，不能再打开</div>
      <template v-if="menu.object.kind === 'table' || menu.object.kind === 'view'">
        <button class="menu-item" type="button" data-action="open-data" :disabled="menu.object.stale" @click="openObject(menu.object, 'data')">
          <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="ICONS.table" /></svg>打开数据
        </button>
        <button class="menu-item" type="button" data-action="open-structure" :disabled="menu.object.stale" @click="openObject(menu.object, 'structure', 'columns')">
          <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="ICONS.structure" /></svg>查看结构
        </button>
        <button class="menu-item" type="button" data-action="new-query" :disabled="menu.object.stale" :title="browseSql(menu.object.engine, menu.object.schema, menu.object.name)" @click="onQuery">
          <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="ICONS.query" /></svg>新建查询
        </button>
      </template>
      <template v-else>
        <button class="menu-item" type="button" data-action="open-definition" :disabled="menu.object.stale" @click="onDefine">
          <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="ICONS.braces" /></svg>查看定义
        </button>
        <button class="menu-item" type="button" data-action="copy-definition" :disabled="menu.object.stale" @click="onCopyDefinition">
          <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="ICONS.copy" /></svg>复制到新查询
        </button>
      </template>
      <button class="menu-item" type="button" data-action="copy-name" @click="onCopy">
        <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="ICONS.tag" /></svg>复制完整名称
      </button>
      <div v-if="menu.notice" class="menu-head">{{ menu.notice }}</div>
      <template v-if="menu.object.kind === 'table'">
        <div class="menu-sep" role="separator"></div>
        <button class="menu-item danger" type="button" data-action="drop-table" :disabled="menu.object.stale" @click="openObject(menu.object, 'structure', 'drop')">
          <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="ICONS.trash" /></svg>删除表…
        </button>
      </template>
    </div>
  </Teleport>
</template>

<style src="./catalog.css"></style>
