<script setup lang="ts">
import { computed, ref, watch } from "vue";
import SchemaBlock from "./SchemaBlock.vue";
import { catalogConnections, catalogLoadError, refreshCatalog, schemaKey } from "./store";
import { engineShape, isConnected, searchObjects, splitMatch } from "./tree";

const props = defineProps<{ filter: string }>();

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

function depthStyle(depth: number) {
  return { "--d": String(depth) };
}
</script>

<template>
  <div class="tree" role="tree" aria-label="数据库对象">
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
        :data-path="hit.path"
        :data-object="hit.name"
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
            <span v-if="hit.env !== 'none'" class="env xs" :class="`env-${hit.env}`">{{ hit.envLabel }}</span>
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
      <section v-for="connection in connections" :key="connection.id" :data-engine="connection.engine">
        <div
          class="tree-row tr-conn"
          :class="`st-${connection.status}`"
          role="treeitem"
          :style="depthStyle(0)"
          :aria-expanded="open(`c:${connection.id}`)"
          :title="`${connection.name} · ${ENGINE_LABEL[connection.engine]} · ${STATUS_TEXT[connection.status]}`"
          @click="toggle(`c:${connection.id}`)"
        >
          <span class="tw">
            <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="open(`c:${connection.id}`) ? ICONS.down : ICONS.right" /></svg>
          </span>
          <span class="eng" :class="`eng-${connection.engine}`">{{ ENGINE_SHORT[connection.engine] }}</span>
          <span class="tr-name">{{ connection.name }}</span>
          <span v-if="connection.env !== 'none'" class="env xs" :class="`env-${connection.env}`">{{ connection.envLabel }}</span>
          <span class="conn-state" :class="`st-${connection.status}`"><i></i><span class="sr">{{ STATUS_TEXT[connection.status] }}</span></span>
        </div>
        <template v-if="open(`c:${connection.id}`)">
          <div v-if="!isConnected(connection.status)" class="tree-note" :class="{ warn: connection.status === 'lost' }" :style="depthStyle(1)">
            <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="connection.status === 'lost' ? ICONS.warn : ICONS.info" /></svg>
            <span>{{ connection.status === "lost" ? "连接已断开，对象列表不再更新" : "未连接，连接后读取对象列表" }}</span>
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
                <div v-if="database.error" class="tree-note err" :style="depthStyle(2)">
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
</template>

<style src="./catalog.css"></style>
