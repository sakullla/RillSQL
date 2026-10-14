<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import type { OpenOutcome } from "../connections/api";
import { focusDatabase, listConnections, openConnection } from "./backend";
import { browseSql, qualifiedObjectName, type CatalogObjectRef } from "./object-ref";
import SchemaBlock from "./SchemaBlock.vue";
import { catalogConnections, catalogLoadError, refreshCatalog, schemaKey } from "./store";
import { engineShape, isConnected, objectPath, postgresSessionGap, searchObjects, splitMatch } from "./tree";
import type { CatalogConnection, DatabaseNode } from "./types";

const props = defineProps<{ filter: string }>();
const emit = defineEmits<{
  "open-object": [object: CatalogObjectRef, pane: "data" | "structure", section?: "columns" | "drop"];
  "query-object": [object: CatalogObjectRef, sql: string];
  "focused-database": [connectionId: string, database: string];
}>();
const menu = ref<{ object: CatalogObjectRef; x: number; y: number; notice: string } | null>(null);
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
  const row = rowOf(event.target);
  const object = row ? readRef(row) : null;
  if (!object) {
    closeMenu();
    return;
  }
  const height = object.kind === "table" ? 210 : 160;
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
  <div class="tree" role="tree" aria-label="数据库对象" @click="onTreeClick" @contextmenu="onContextMenu" @scroll="closeMenu">
    <div class="tree-toolbar">
      <button class="link" type="button" data-action="tree-refresh" aria-label="刷新对象列表" @click="refreshCatalog()">刷新对象列表</button>
    </div>
    <div v-if="loadError" class="tree-note err" data-load-error>
      <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="ICONS.error" /></svg>
      <div>
        <strong>无法读取对象列表</strong>
        <span>{{ loadError }}</span>
        <button class="link" type="button" data-action="tree-retry" @click="refreshCatalog()">重试</button>
      </div>
    </div>
    <template v-if="filter.trim()">
      <div class="tree-filter-head">匹配 {{ matches.hits.length }} 个对象</div>
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
            <span v-if="hit.stale" class="pill pill-err">已失效</span>
          </span>
        </span>
      </div>
      <div v-if="matches.hits.length === 0" class="tree-note" style="--d: 0">
        <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="ICONS.info" /></svg>
        <span>已连接的数据库中没有名称包含“{{ filter.trim() }}”的表</span>
      </div>
      <div v-if="matches.skipped.length" class="tree-note subtle" style="--d: 0" data-skipped>
        <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="ICONS.info" /></svg>
        <span>{{ matches.skipped.join("、") }} 未连接，未包含在结果中</span>
      </div>
    </template>
    <p v-else-if="connections.length === 0 && !loadError" class="tree-note">还没有连接。连接后按引擎显示数据库、schema 和表。</p>
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
              class="link"
              type="button"
              data-action="tree-connect"
              :data-connection="connection.id"
              :disabled="connecting === connection.id || connection.status === 'connecting'"
              @click.stop="connectSession(connection)"
            >
              {{ connecting === connection.id ? "正在连接…" : connection.status === "lost" ? "重新连接" : "连接" }}
            </button>
            <span class="conn-state" :class="`st-${connection.status}`"><i></i><span class="sr">{{ STATUS_TEXT[connection.status] }}</span></span>
          </span>
        </div>
        <template v-if="open(`c:${connection.id}`)">
          <div v-if="!isConnected(connection.status)" class="tree-note" :class="{ warn: connection.status === 'lost' }" :style="depthStyle(1)" data-disconnected>
            <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="connection.status === 'lost' ? ICONS.warn : ICONS.info" /></svg>
            <div>
              <span>{{ connection.status === "lost" ? "连接已断开，对象列表不再更新" : "未连接，连接后读取对象列表" }}</span>
              <form v-if="passwordFor === connection.id" class="tree-connect" @submit.prevent="connectSession(connection, typedPassword)">
                <input v-model="typedPassword" type="password" autocomplete="current-password" aria-label="数据库密码" />
                <button class="link" type="submit" data-action="tree-connect" :data-connection="connection.id" :disabled="connecting === connection.id">连接</button>
              </form>
              <span v-if="connectError && connectTarget === connection.id" data-connect-error>{{ connectError }}</span>
            </div>
          </div>
          <template v-else>
          <div v-if="connection.error" class="tree-note err" data-connection-error :style="depthStyle(1)">
            <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="ICONS.error" /></svg>
            <div>
              <strong>无法读取对象列表</strong>
              <span>{{ connection.error }}</span>
              <button class="link" type="button" data-action="tree-retry" @click="refreshCatalog()">重试</button>
            </div>
          </div>
          <template v-if="!engineShape(connection.engine).databases">
            <div v-if="connection.foreignKeys !== null" class="tree-note subtle" :style="depthStyle(1)" data-foreign-keys>
              <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="ICONS.info" /></svg>
              <span>foreign_keys = {{ connection.foreignKeys ? "ON" : "OFF" }}</span>
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
                <span v-if="database.name === connection.defaultDatabase" class="tr-meta">默认</span>
              </div>
              <template v-if="open(`d:${connection.id}:${database.name}`)">
                <div v-if="sessionGap(database)" class="tree-note" :style="depthStyle(2)" data-other-database>
                  <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="ICONS.info" /></svg>
                  <div>
                    <span>这条会话连在 {{ sessionGap(database)?.current }}。PostgreSQL 一次只能读取一个库。</span>
                    <button
                      class="link"
                      type="button"
                      data-action="focus-database"
                      :data-database="database.name"
                      :data-connection="connection.id"
                      :disabled="focusing === focusKey(connection.id, database.name)"
                      title="把当前会话改连到这个库。正在执行的语句会断开。已保存的连接配置不变。"
                      @click.stop="focusSession(connection, database.name)"
                    >
                      {{ focusing === focusKey(connection.id, database.name) ? "正在连接…" : `连接到 ${database.name}` }}
                    </button>
                    <span v-if="focusError && focusTarget === focusKey(connection.id, database.name)" data-focus-error>{{ focusError }}</span>
                  </div>
                </div>
                <div v-else-if="database.error" class="tree-note err" :style="depthStyle(2)">
                  <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="ICONS.error" /></svg>
                  <div>
                    <strong>无法读取对象列表</strong>
                    <span>{{ database.error }}</span>
                    <button class="link" type="button" data-action="tree-retry" @click="refreshCatalog()">重试</button>
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
      <div v-if="menu.object.stale" class="menu-head">这个对象已失效</div>
      <button class="menu-item" type="button" data-action="open-data" :disabled="menu.object.stale" @click="openObject(menu.object, 'data')">打开数据</button>
      <button class="menu-item" type="button" data-action="open-structure" :disabled="menu.object.stale" @click="openObject(menu.object, 'structure', 'columns')">查看结构</button>
      <button class="menu-item" type="button" data-action="new-query" :disabled="menu.object.stale" :title="browseSql(menu.object.engine, menu.object.schema, menu.object.name)" @click="onQuery">新建查询</button>
      <button class="menu-item" type="button" data-action="copy-name" @click="onCopy">复制完整名称</button>
      <div v-if="menu.notice" class="menu-head">{{ menu.notice }}</div>
      <template v-if="menu.object.kind === 'table'">
        <div class="menu-sep" role="separator"></div>
        <button class="menu-item danger" type="button" data-action="drop-table" :disabled="menu.object.stale" @click="openObject(menu.object, 'structure', 'drop')">删除表…</button>
      </template>
    </div>
  </Teleport>
</template>

<style src="./catalog.css"></style>
