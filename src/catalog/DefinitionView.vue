<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { tr as t } from "../i18n";
import type { Engine } from "../engines";
import Listbox from "../ui/Listbox.vue";
import type { ListboxOption } from "../ui/types";
import { executeStructure, previewStructure, type StructureRequest } from "../structure/api";
import ChangePreviewDialog from "../structure/ChangePreviewDialog.vue";
import type { StructureAction, StructurePlan, StructureReceipt } from "../structure/types";
import { fetchDefinition } from "./backend";
import type { CatalogObjectRef } from "./object-ref";
import { catalogConnections } from "./store";
import { kindLabel } from "./tree";
import type { ObjectKind } from "./types";

const props = defineProps<{
  object: Pick<CatalogObjectRef, "connectionId" | "database" | "schema" | "name" | "kind" | "signature" | "relation">;
  label: string;
  /** 目录里这条连接的引擎；不给时从目录快照里查。 */
  engine?: Engine;
}>();
const emit = defineEmits<{ "copy-sql": [sql: string] }>();

const sql = ref("");
const error = ref("");
const loading = ref(true);

const ICONS: Record<ObjectKind, string> = {
  table: "M4 4h16a1 1 0 0 1 1 1v14a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V5a1 1 0 0 1 1-1ZM3 10h18M10 10v10",
  view: "M2 12s3.6-7 10-7 10 7 10 7-3.6 7-10 7S2 12 2 12ZM9 12a3 3 0 1 0 6 0 3 3 0 1 0-6 0",
  function: "M8 7h3M8 12h3M8 17h3M14 7l2 5-2 5M4 4h16v16H4z",
  procedure: "M6 7h12M6 12h12M6 17h8M4 4h16v16H4z",
  trigger: "M12 3v4M12 17v4M4.9 6.5l3.5 2M15.6 15.5l3.5 2M4.9 17.5l3.5-2M15.6 8.5l3.5-2",
  sequence: "M4 7h16M4 12h10M4 17h16",
  enum: "M5 5h6v6H5zM13 5h6v6h-6zM5 13h6v6H5zM13 13h6v6h-6z",
  domain: "M4 6h16v12H4zM8 10h8M8 14h5",
  extension: "M10 3h4v7h7v4h-7v7h-4v-7H3v-4h7z",
  event: "M12 4a8 8 0 1 1 0 16 8 8 0 0 1 0-16M12 8v4.5l3 2",
};

/** 错误态图标独立于按 ObjectKind 索引的映射，不进 ICONS。 */
const ERROR_ICON = "M2 12a10 10 0 1 0 20 0 10 10 0 1 0-20 0M15 9l-6 6M9 9l6 6";

const connection = computed(() => catalogConnections().find((item) => item.id === props.object.connectionId) ?? null);
const engine = computed<Engine | null>(() => props.engine ?? connection.value?.engine ?? null);
const environmentLabel = computed(() => connection.value?.envLabel.trim() ?? "");
const environmentTone = computed(() => connection.value?.envTone ?? "none");

/** 枚举与域在 PostgreSQL 上有内容可改：接 T9 的 addEnumValue / alterDomain，先预览再执行。 */
const editable = computed(() => engine.value === "postgres" && (props.object.kind === "enum" || props.object.kind === "domain"));

const enumValue = ref("");
const enumBefore = ref("");
const domainDefault = ref("");
const domainNotNull = ref<"keep" | "set" | "clear">("keep");
const editError = ref("");
const plan = ref<StructurePlan | null>(null);
const receipt = ref<StructureReceipt | null>(null);
const pending = ref<StructureAction | null>(null);
const busy = ref(false);

const notNullOptions = computed<ListboxOption[]>(() => [
  { value: "keep", label: t("catalog.definition.notNullKeep") },
  { value: "set", label: t("catalog.definition.notNullSet") },
  { value: "clear", label: t("catalog.definition.notNullClear") },
]);

async function load(): Promise<void> {
  loading.value = true;
  try {
    sql.value = await fetchDefinition(props.object);
    error.value = "";
  } catch (caught) {
    error.value =
      caught instanceof Error && caught.message
        ? caught.message
        : typeof caught === "string" && caught
          ? caught
          : t("catalog.definition.failed");
    sql.value = "";
  } finally {
    loading.value = false;
  }
}

onMounted(load);

function requestOf(): StructureRequest | null {
  if (!engine.value) return null;
  return {
    connectionId: props.object.connectionId,
    database: props.object.database,
    schema: props.object.schema,
    engine: engine.value,
  };
}

function openPreview(action: StructureAction): void {
  const request = requestOf();
  if (!request) {
    editError.value = t("catalog.definition.editNeedsConnection");
    return;
  }
  busy.value = true;
  editError.value = "";
  receipt.value = null;
  pending.value = action;
  previewStructure(request, action)
    .then((next) => {
      plan.value = next;
    })
    .catch((caught: unknown) => {
      plan.value = null;
      pending.value = null;
      editError.value = caught instanceof Error && caught.message ? caught.message : String(caught);
    })
    .finally(() => {
      busy.value = false;
    });
}

function previewEnum(): void {
  const value = enumValue.value.trim();
  if (!value) {
    editError.value = t("catalog.definition.enumValueRequired");
    return;
  }
  const before = enumBefore.value.trim();
  openPreview({ kind: "addEnumValue", enumName: props.object.name, value, before: before || null });
}

function previewDomain(): void {
  const defaultSql = domainDefault.value.trim();
  const notNull = domainNotNull.value === "keep" ? null : domainNotNull.value === "set";
  if (defaultSql === "" && notNull === null) {
    editError.value = t("catalog.definition.domainNoChange");
    return;
  }
  // 空串表示「不改默认值」：只有填了内容才进语句，避免顺手把已有默认值删掉。
  openPreview({ kind: "alterDomain", domain: props.object.name, defaultSql: defaultSql || null, notNull });
}

async function confirm(): Promise<void> {
  const request = requestOf();
  const action = pending.value;
  const current = plan.value;
  if (!request || !action || !current || current.blocked.length > 0 || current.steps.length === 0) return;
  busy.value = true;
  try {
    receipt.value = await executeStructure(request, action, current.steps.map((step) => step.sql));
    const failed = receipt.value.blocked.length > 0 || receipt.value.steps.some((step) => step.status !== "ok");
    if (!failed) {
      enumValue.value = "";
      enumBefore.value = "";
      await load();
    }
  } catch (caught) {
    receipt.value = {
      blocked: [],
      rule: current.rule,
      target: current.target,
      rebuild: current.rebuild,
      steps: [
        {
          sql: "",
          label: t("catalog.definition.executeLabel"),
          status: "unknown",
          message: caught instanceof Error ? caught.message : String(caught),
        },
      ],
    };
  } finally {
    busy.value = false;
  }
}

function closePreview(): void {
  plan.value = null;
  receipt.value = null;
  pending.value = null;
}

function copy() {
  if (!sql.value.trim()) return;
  emit("copy-sql", sql.value);
}
</script>

<template>
  <section class="definition-view" data-definition :data-kind="object.kind">
    <header class="definition-bar">
      <span class="definition-kind" aria-hidden="true">
        <svg class="i" viewBox="0 0 24 24"><path :d="ICONS[object.kind]" /></svg>
      </span>
      <div class="definition-titles">
        <h2>{{ label }}</h2>
        <p>{{ kindLabel(object.kind) }} · {{ editable ? t("catalog.definition.editable") : t("catalog.definition.readOnly") }}</p>
      </div>
      <button v-if="sql" class="btn sm" type="button" data-action="copy-definition" @click="copy">{{ t("catalog.definition.copy") }}</button>
    </header>
    <p v-if="error" class="definition-error" data-definition-error>
      <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="ERROR_ICON" /></svg>
      <span>{{ error }}</span>
    </p>
    <div v-else-if="loading" class="definition-loading" data-definition-loading>
      <span class="definition-spin" aria-hidden="true"></span>
      {{ t("catalog.definition.loading") }}
    </div>
    <pre v-else class="definition-sql" data-definition-sql data-readonly="true">{{ sql }}</pre>

    <form v-if="editable" class="definition-edit" data-definition-edit :data-edit-kind="object.kind" @submit.prevent="object.kind === 'enum' ? previewEnum() : previewDomain()">
      <h3>{{ t("catalog.definition.editTitle") }}</h3>
      <template v-if="object.kind === 'enum'">
        <p class="definition-edit-hint">{{ t("catalog.definition.enumHint") }}</p>
        <label class="definition-field">
          <span>{{ t("catalog.definition.enumValue") }}</span>
          <input v-model="enumValue" type="text" data-field="enum-value" :aria-label="t('catalog.definition.enumValueAria')" :placeholder="t('catalog.definition.enumValuePlaceholder')" />
        </label>
        <label class="definition-field">
          <span>{{ t("catalog.definition.enumBefore") }}</span>
          <input v-model="enumBefore" type="text" data-field="enum-before" :aria-label="t('catalog.definition.enumBeforeAria')" :placeholder="t('catalog.definition.enumBeforePlaceholder')" />
        </label>
      </template>
      <template v-else>
        <p class="definition-edit-hint">{{ t("catalog.definition.domainHint") }}</p>
        <label class="definition-field">
          <span>{{ t("catalog.definition.domainDefault") }}</span>
          <input v-model="domainDefault" type="text" data-field="domain-default" :aria-label="t('catalog.definition.domainDefaultAria')" :placeholder="t('catalog.definition.domainDefaultPlaceholder')" />
        </label>
        <label class="definition-field">
          <span>{{ t("catalog.definition.domainNotNull") }}</span>
          <Listbox v-model="domainNotNull" :options="notNullOptions" :aria-label="t('catalog.definition.domainNotNullAria')" />
        </label>
      </template>
      <p v-if="editError" class="definition-error" role="alert" data-edit-error>{{ editError }}</p>
      <div class="definition-edit-actions">
        <button class="btn primary sm" type="submit" data-action="preview-definition-edit" :disabled="busy">
          {{ t("catalog.definition.previewEdit") }}
        </button>
      </div>
    </form>

    <ChangePreviewDialog
      v-if="plan"
      :plan="plan"
      :receipt="receipt"
      :busy="busy"
      :engine="engine ?? ''"
      :connection-name="connection?.name ?? ''"
      :env-label="environmentLabel"
      :env-tone="environmentTone"
      @cancel="closePreview"
      @confirm="confirm"
    />
  </section>
</template>

<style scoped>
.definition-view { display: flex; flex-direction: column; gap: var(--sp-3); height: 100%; min-height: 0; padding: var(--sp-4); }
.definition-bar { display: flex; align-items: center; gap: var(--sp-3); }
.definition-kind {
  display: grid;
  place-items: center;
  width: 28px;
  height: 28px;
  border-radius: var(--r-sm);
  background: var(--accent-soft);
  color: var(--accent-text);
  box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--accent) 20%, transparent);
  flex-shrink: 0;
}
.definition-kind .i { width: 15px; height: 15px; }
.definition-titles { flex: 1; min-width: 0; }
.definition-bar h2 { margin: 0; font-size: var(--fs-md); font-weight: 600; letter-spacing: -0.01em; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.definition-titles p { margin: 2px 0 0; font-size: var(--fs-xs); color: var(--text-3); }
.definition-error {
  display: flex;
  align-items: flex-start;
  gap: var(--sp-2);
  margin: 0;
  padding: var(--sp-3);
  border-radius: var(--r-md);
  background: var(--err-soft);
  box-shadow: inset 0 0 0 1px var(--err-line);
  color: var(--err);
  font-size: var(--fs-sm);
  line-height: var(--lh);
  white-space: pre-wrap;
}
.definition-error .i { width: 14px; height: 14px; flex-shrink: 0; margin-top: 2px; }
.definition-loading { display: flex; align-items: center; gap: var(--sp-2); color: var(--text-2); font-size: var(--fs-sm); }
.definition-spin { width: 12px; height: 12px; border-radius: var(--r-full); border: 1.5px solid var(--line-2); border-top-color: var(--accent); animation: definition-spin 0.8s linear infinite; }
.definition-sql {
  flex: 1;
  overflow: auto;
  margin: 0;
  padding: var(--sp-4);
  border: 1px solid var(--stroke);
  border-radius: var(--r-md);
  background: var(--surface-sunken);
  color: var(--text);
  font: var(--fs-sm)/1.6 var(--mono);
  white-space: pre;
  box-shadow: inset 0 1px 2px color-mix(in srgb, var(--press) 25%, transparent);
}
.definition-edit {
  display: flex;
  flex-direction: column;
  gap: var(--sp-2);
  padding: var(--sp-3);
  border: 1px solid var(--stroke);
  border-radius: var(--r-md);
  background: var(--surface-panel);
}
.definition-edit h3 { margin: 0; font-size: var(--fs-sm); font-weight: 600; }
.definition-edit-hint { margin: 0; color: var(--text-3); font-size: var(--fs-xs); }
.definition-field { display: flex; flex-direction: column; gap: 2px; font-size: var(--fs-xs); color: var(--text-3); }
.definition-field input {
  padding: 4px 8px;
  border: 1px solid var(--line);
  border-radius: var(--r-sm);
  background: var(--surface);
  color: var(--text);
  font: inherit;
  font-size: var(--fs-sm);
}
.definition-edit-actions { display: flex; gap: var(--sp-2); }
@keyframes definition-spin { to { transform: rotate(1turn); } }
</style>
