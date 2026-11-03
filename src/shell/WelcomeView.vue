<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { catalogConnections } from "../catalog/store";
import { queryHistory, querySavedList } from "../history/api";
import { openPlan, type HistoryEntry, type OpenPlan } from "../history/plan";
import type { SavedQuery } from "../history/saved";
import { tr as t } from "../i18n";
import BrandMark from "./BrandMark.vue";
import ChordKeys from "./ChordKeys.vue";
import Icon from "./Icon.vue";

const RECENT_LIMIT = 6;
const SAVED_LIMIT = 4;

const emit = defineEmits<{
  "new-query": [];
  connections: [];
  sqlite: [];
  compare: [];
  "history-open": [plan: OpenPlan];
  "show-history": [section: "runs" | "saved"];
  help: [];
}>();
const saved = computed(() => catalogConnections().length > 0);
const knownIds = computed(() => new Set(catalogConnections().map((item) => item.id)));
const recent = ref<HistoryEntry[]>([]);
const savedQueries = ref<SavedQuery[]>([]);

/** 首页只是便捷入口：读不到历史时安静留空，不在这里报错。 */
onMounted(async () => {
  try {
    recent.value = (await queryHistory()).slice(0, RECENT_LIMIT);
  } catch {
    recent.value = [];
  }
  try {
    savedQueries.value = (await querySavedList()).slice(0, SAVED_LIMIT);
  } catch {
    savedQueries.value = [];
  }
});

function sqlLine(sql: string): string {
  return sql.replace(/\s+/g, " ").trim();
}

function reopen(entry: HistoryEntry) {
  emit("history-open", openPlan(entry, knownIds.value));
}

function reopenSaved(item: SavedQuery) {
  const exists = knownIds.value.has(item.connectionId);
  emit("history-open", {
    sql: item.sql,
    connectionId: exists ? item.connectionId : null,
    connectionName: item.connectionName,
    database: exists ? item.databaseName : null,
    engine: item.engine,
    needsTarget: !exists,
    execute: false,
  });
}
</script>

<template>
  <div class="welcome">
    <div class="welcome-inner">
      <template v-if="saved">
        <header class="desk-heading">
          <div>
            <h1>{{ t("shell.welcome.resumeTitle") }}</h1>
            <p>{{ t("shell.welcome.resumeHint") }}</p>
          </div>
          <button class="btn primary" type="button" :aria-label="t('common.newQuery')" @click="emit('new-query')"><Icon name="plus" />{{ t("common.newQuery") }}<ChordKeys chord="Mod+T" /></button>
        </header>
        <div class="welcome-quick">
          <button class="btn resume-action" type="button" :title="t('shell.welcome.newConnectionHint')" @click="emit('connections')"><span class="action-icon"><Icon name="database" /></span><span><b>{{ t("common.newConnection") }}</b><small>{{ t("shell.welcome.newConnectionHint") }}</small></span></button>
          <button class="btn resume-action" type="button" :title="t('shell.welcome.openSqliteHint')" @click="emit('sqlite')"><span class="action-icon"><Icon name="file" /></span><span><b>{{ t("common.openSqlite") }}</b><small>{{ t("shell.welcome.openSqliteHint") }}</small></span></button>
          <button class="btn resume-action" type="button" :title="t('shell.welcome.compareHint')" @click="emit('compare')"><span class="action-icon"><Icon name="columns" /></span><span><b>{{ t("shell.welcome.compare") }}</b><small>{{ t("shell.welcome.compareHint") }}</small></span></button>
        </div>
        <div class="desk-columns">
          <section :aria-label="t('shell.welcome.recent')">
            <div class="section-heading">
              <h2>{{ t("shell.welcome.recent") }}</h2>
              <button class="desk-link" type="button" @click="emit('show-history', 'runs')">{{ t("shell.welcome.viewAll") }}</button>
            </div>
            <p v-if="!recent.length" class="desk-none">{{ t("shell.welcome.recentEmpty") }}</p>
            <ul v-else class="desk-list">
              <li v-for="entry in recent" :key="entry.id">
                <button class="desk-entry" type="button" data-action="welcome-recent" :title="entry.sql" @click="reopen(entry)">
                  <code>{{ sqlLine(entry.sql) }}</code>
                  <small>{{ entry.connectionName }}{{ entry.databaseName ? ` / ${entry.databaseName}` : "" }}</small>
                </button>
              </li>
            </ul>
          </section>
          <div class="desk-side">
            <section :aria-label="t('shell.welcome.saved')">
              <div class="section-heading">
                <h2>{{ t("shell.welcome.saved") }}</h2>
                <button class="desk-link" type="button" @click="emit('show-history', 'saved')">{{ t("shell.welcome.viewAll") }}</button>
              </div>
              <p v-if="!savedQueries.length" class="desk-none">{{ t("shell.welcome.savedEmpty") }}</p>
              <ul v-else class="desk-list">
                <li v-for="item in savedQueries" :key="item.id">
                  <button class="desk-entry" type="button" data-action="welcome-saved" :title="item.sql" @click="reopenSaved(item)">
                    <b>{{ item.name }}</b>
                    <small>{{ item.connectionName }}{{ item.databaseName ? ` / ${item.databaseName}` : "" }}</small>
                  </button>
                </li>
              </ul>
            </section>
            <section :aria-label="t('shell.welcome.keys')">
              <div class="section-heading">
                <h2>{{ t("shell.welcome.keys") }}</h2>
                <button class="desk-link" type="button" @click="emit('help')">{{ t("shell.welcome.viewAll") }}</button>
              </div>
              <ul class="desk-keys">
                <li><span>{{ t("shell.welcome.keyPalette") }}</span><ChordKeys chord="Mod+K" /></li>
                <li><span>{{ t("shell.welcome.keyTables") }}</span><ChordKeys chord="Mod+P" /></li>
                <li><span>{{ t("shell.welcome.keyRun") }}</span><ChordKeys chord="Mod+Enter" /></li>
              </ul>
            </section>
          </div>
        </div>
      </template>
      <template v-else>
        <header class="welcome-hero">
          <BrandMark large />
          <div class="hero-copy">
            <p class="hero-kicker">{{ t("shell.welcome.kicker") }}</p>
            <h1>{{ t("shell.welcome.title") }}</h1>
            <p>{{ t("shell.welcome.subtitle") }}</p>
          </div>
        </header>
        <div class="desk-actions">
          <button class="btn primary desk-action" type="button" :title="t('shell.welcome.newConnectionHint')" @click="emit('connections')"><span class="action-icon"><Icon name="database" /></span><span><b>{{ t("common.newConnection") }}</b><small>{{ t("shell.welcome.newConnectionHint") }}</small></span></button>
          <button class="btn desk-action" type="button" :title="t('shell.welcome.openSqliteHint')" @click="emit('sqlite')"><span class="action-icon"><Icon name="file" /></span><span><b>{{ t("common.openSqlite") }}</b><small>{{ t("shell.welcome.openSqliteHint") }}</small></span></button>
        </div>
        <ul class="hero-facts">
          <li><Icon name="check" />{{ t("shell.welcome.factLocal") }}</li>
          <li><Icon name="check" />{{ t("shell.welcome.factKeyring") }}</li>
          <li><Icon name="check" />{{ t("shell.welcome.factEngines") }}</li>
        </ul>
      </template>
    </div>
  </div>
</template>

<style scoped>
.welcome-quick { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: var(--sp-3); margin-bottom: var(--sp-7); }
.resume-action.btn {
  height: auto;
  width: 100%;
  min-width: 0;
  padding: var(--sp-3);
  gap: var(--sp-3);
  justify-content: flex-start;
  text-align: left;
  white-space: normal;
  border-radius: var(--r-lg);
  transition: border-color var(--dur-fast) var(--ease), box-shadow var(--dur-fast) var(--ease), background var(--dur-fast) var(--ease);
}
.resume-action.btn:hover:not(:disabled) { background: var(--surface-panel); border-color: color-mix(in srgb, var(--accent) 50%, var(--line-2)); box-shadow: var(--shadow); }
.resume-action b { display: block; font-size: var(--fs-md); font-weight: 600; margin-bottom: 2px; }
.resume-action > span:last-child { min-width: 0; overflow: hidden; }
.resume-action small { display: block; color: var(--text-2); font-size: var(--fs-xs); font-weight: 400; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.desk-action.primary .action-icon { background: color-mix(in srgb, var(--on-accent) 18%, transparent); color: var(--on-accent); }
.desk-action.primary b, .desk-action.primary small { color: var(--on-accent); }
.desk-heading .btn :deep(kbd) { background: transparent; color: inherit; border-color: currentColor; opacity: .7; box-shadow: none; }
.desk-actions { grid-template-columns: repeat(2, minmax(0, 1fr)); }
@media (max-width: 960px) {
  .welcome-quick { grid-template-columns: 1fr; gap: var(--sp-2); margin-bottom: var(--sp-6); }
  .desk-actions { grid-template-columns: 1fr; }
}
</style>
