<script setup lang="ts">
import { tr as t } from "../i18n";
import { onMounted, ref } from "vue";
import type { CatalogConnection } from "../catalog/types";
import { failureOf, tauriRedisApi, type RedisApi, type RedisKeyInfo } from "./api";
import { describeTtl, keyTypeBadge, mergeScanPage } from "./keys";

const props = defineProps<{ connection: CatalogConnection; api?: RedisApi }>();
const emit = defineEmits<{ "open-key": [connection: CatalogConnection, key: string] }>();

const api = ref(props.api ?? tauriRedisApi);
const keys = ref<RedisKeyInfo[]>([]);
const cursor = ref(0);
const done = ref(false);
const loading = ref(false);
const error = ref("");

async function loadMore() {
  if (loading.value || done.value) return;
  loading.value = true;
  error.value = "";
  try {
    const page = await api.value.scanKeys(props.connection.id, cursor.value, null);
    const merged = mergeScanPage(keys.value, page);
    keys.value = merged.keys;
    cursor.value = merged.cursor;
    done.value = merged.done;
  } catch (cause) {
    error.value = failureOf(cause);
  } finally {
    loading.value = false;
  }
}

function openKey(item: RedisKeyInfo) {
  emit("open-key", props.connection, item.name);
}

onMounted(loadMore);
</script>

<!-- 树里的 Redis 键空间（R11/ADR-4）：连接节点下直接展开 SCAN 分页列表；双击键开键浏览器标签。 -->
<template>
  <div class="rtk" :data-connection="connection.id" data-redis-tree>
    <div class="tree-row rtk-head" :style="{ '--d': 1 }">
      <span class="tw"></span>
      <span class="tr-name">{{ t("redis.tab.keyspaceTree", { n: keys.length }) }}</span>
    </div>
    <p v-if="error" class="rtk-error">{{ error }}</p>
    <button
      v-for="item in keys"
      :key="item.name"
      class="tree-row"
      type="button"
      data-level="key"
      :data-key="item.name"
      :data-type="keyTypeBadge(item.keyType)"
      :style="{ '--d': 2 }"
      :title="`${item.name} · ${keyTypeBadge(item.keyType)} · ${describeTtl(item.ttlMs)}`"
      @dblclick="openKey(item)"
    >
      <span class="tw"></span>
      <span class="rtk-badge" :data-type="keyTypeBadge(item.keyType)">{{ keyTypeBadge(item.keyType) }}</span>
      <span class="tr-name mono">{{ item.name }}</span>
    </button>
    <button v-if="!done" class="tree-row rtk-more" type="button" data-action="tree-more" :style="{ '--d': 2 }" :disabled="loading" @click="loadMore">
      <span class="tw"></span>
      <span class="tr-name">{{ loading ? t("redis.tab.loading") : t("redis.tab.showMoreTree") }}</span>
    </button>
  </div>
</template>

<style scoped>
.rtk { display: contents; }
.rtk-error { color: var(--danger, #c0392b); font-size: 12px; margin: 0; padding: var(--sp-1) var(--sp-2); }
.rtk-badge { font-size: 10px; padding: 0 5px; border-radius: 999px; border: 1px solid var(--line); color: var(--text-3); white-space: nowrap; }
.rtk-more { color: var(--text-3); }
.mono { font-family: var(--mono, ui-monospace, monospace); }
</style>
