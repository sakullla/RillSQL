<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue";
import { fetchExtras } from "../catalog/backend";
import type { CatalogExtras, SequenceInfo } from "../catalog/types";
import type { Engine } from "../engines";
import { tr as t } from "../i18n";
import Icon from "../shell/Icon.vue";
import ChangePreviewDialog from "../structure/ChangePreviewDialog.vue";
import type { StructurePlan, StructureReceipt } from "../structure/types";
import Listbox from "../ui/Listbox.vue";
import type { ListboxOption } from "../ui/types";
import { toast } from "../ui/toast";
import { executeDatabaseAction, previewDatabaseAction } from "./api";
import { JOURNAL_MODES, adminSections, sequenceAction, sequenceDraft } from "./objects";
import type { DatabaseAction, SequenceDraft } from "./types";

const props = defineProps<{
  connectionId: string;
  connectionName: string;
  database: string;
  engine: Engine;
  schema?: string;
  envLabel?: string;
  envTone?: string;
}>();

const sections = computed(() => adminSections(props.engine));
const extras = ref<CatalogExtras | null>(null);
const loading = ref(false);
const failure = ref("");
const busy = ref(false);
const plan = ref<StructurePlan | null>(null);
const receipt = ref<StructureReceipt | null>(null);
const pending = ref<DatabaseAction | null>(null);
const pendingSchema = ref("");

function reasonOf(caught: unknown): string {
  if (caught instanceof Error && caught.message) return caught.message;
  if (typeof caught === "string" && caught) return caught;
  return t("admin.panel.unknownError");
}

function requestBase(schema: string) {
  return {
    connectionId: props.connectionId,
    database: props.database,
    schema,
    engine: String(props.engine),
  };
}

/** 读对象详情：一次拿到 PRAGMA/附加库/扩展/序列/事件/分区，分节各自的失败原因留在分节里。 */
async function load(): Promise<void> {
  loading.value = true;
  try {
    const next = await fetchExtras({
      connectionId: props.connectionId,
      database: props.database,
      schema: props.schema ?? "",
    });
    if (next) {
      extras.value = next;
      failure.value = "";
    } else {
      failure.value = t("admin.panel.loadFailed", { reason: t("admin.panel.unknownError") });
    }
  } catch (caught) {
    failure.value = t("admin.panel.loadFailed", { reason: reasonOf(caught) });
  } finally {
    loading.value = false;
  }
}

/** 写动作统一走预览：先要语句，确认后才执行，执行完重新读一遍对象详情。 */
async function request(action: DatabaseAction, schema = props.schema ?? ""): Promise<void> {
  if (busy.value) return;
  busy.value = true;
  try {
    plan.value = await previewDatabaseAction(requestBase(schema), action);
    pending.value = action;
    pendingSchema.value = schema;
    receipt.value = null;
  } catch (caught) {
    toast(t("admin.panel.previewFailed", { reason: reasonOf(caught) }), "err");
  } finally {
    busy.value = false;
  }
}

async function confirmRun(): Promise<void> {
  const action = pending.value;
  const current = plan.value;
  if (!action || !current) return;
  busy.value = true;
  try {
    receipt.value = await executeDatabaseAction(
      requestBase(pendingSchema.value),
      action,
      current.steps.map((step) => step.sql),
    );
    const failed = receipt.value.blocked.length > 0 || receipt.value.steps.some((step) => step.status !== "ok");
    if (!failed) toast(t("admin.maintenance.done"), "ok");
    await load();
  } catch (caught) {
    toast(t("admin.panel.executeFailed", { reason: reasonOf(caught) }), "err");
    dismiss();
  } finally {
    busy.value = false;
  }
}

function dismiss(): void {
  plan.value = null;
  receipt.value = null;
  pending.value = null;
}

const pragmas = computed(() => extras.value?.pragmas ?? null);
const foreignKeys = computed(() => pragmas.value?.foreignKeys ?? null);
const foreignKeyText = computed(() =>
  foreignKeys.value === null ? t("admin.pragma.unknown") : foreignKeys.value ? t("admin.pragma.on") : t("admin.pragma.off"),
);
const attached = computed(() => extras.value?.attached.items ?? []);
const extensions = computed(() => extras.value?.extensions.items ?? []);
const sequences = computed(() => extras.value?.sequences.items ?? []);
const events = computed(() => extras.value?.events.items ?? []);
const partitions = computed(() => extras.value?.partitions.items ?? []);

const journal = ref<string | null>(null);
watch(
  () => pragmas.value?.journalMode ?? null,
  (mode) => {
    journal.value = mode ? mode.toLowerCase() : null;
  },
  { immediate: true },
);
const journalOptions = computed<ListboxOption[]>(() =>
  JOURNAL_MODES.map((mode) => ({ value: mode, label: mode })),
);

const userVersion = ref("");
watch(
  () => pragmas.value?.userVersion ?? null,
  (version) => {
    userVersion.value = version === null ? "" : String(version);
  },
  { immediate: true },
);

const attachPath = ref("");
const attachName = ref("");
const reindexTarget = ref("");

function toggleForeignKeys(): void {
  const current = foreignKeys.value;
  if (current === null) return;
  void request({ kind: "setForeignKeys", enabled: !current });
}

function saveJournalMode(): void {
  if (!journal.value) return;
  void request({ kind: "setPragma", pragma: "journal_mode", value: journal.value });
}

function saveUserVersion(): void {
  const value = userVersion.value.trim();
  if (!/^\d+$/.test(value)) {
    toast(t("admin.pragma.userVersionInvalid"), "err");
    return;
  }
  void request({ kind: "setPragma", pragma: "user_version", value });
}

function attachDatabase(): void {
  if (!attachPath.value.trim() || !attachName.value.trim()) {
    toast(t("admin.attached.missing"), "err");
    return;
  }
  void request({ kind: "attachDatabase", path: attachPath.value.trim(), name: attachName.value.trim() });
}

function vacuum(): void {
  void request({ kind: "vacuum" });
}

function reindex(): void {
  const target = reindexTarget.value.trim();
  void request({ kind: "reindex", target: target || null });
}

function toggleEvent(name: string, enabled: boolean): void {
  void request({ kind: "setEventEnabled", event: name, enabled });
}

const editing = ref<SequenceInfo | null>(null);
const draft = ref<SequenceDraft>({ increment: "", minValue: "", maxValue: "", start: "", cycle: false });

function startEdit(sequence: SequenceInfo): void {
  editing.value = sequence;
  draft.value = sequenceDraft(sequence);
}

function cancelEdit(): void {
  editing.value = null;
}

function previewSequence(): void {
  const source = editing.value;
  if (!source) return;
  const change = sequenceAction(source, draft.value);
  if (!change) {
    toast(t("admin.sequences.changed"), "err");
    return;
  }
  void request(change.action, change.schema);
  editing.value = null;
}

onMounted(() => {
  void load();
});

watch(
  () => [props.connectionId, props.database, props.schema ?? ""],
  () => {
    editing.value = null;
    dismiss();
    void load();
  },
);
</script>

<template>
  <section class="admin-panel" data-admin-panel :data-engine="engine">
    <header class="admin-bar">
      <span class="admin-icon" aria-hidden="true"><Icon name="database" /></span>
      <div class="admin-titles">
        <h2>{{ t("admin.panel.area") }}</h2>
        <p>
          {{ t("admin.panel.target") }}<template v-if="connectionName"> · {{ connectionName }}</template>
          <template v-if="database"> / {{ database }}</template>
          <template v-if="schema"> / {{ schema }}</template>
          <span v-if="envLabel" class="env xs" :class="envTone && envTone !== 'none' ? `env-${envTone}` : undefined">{{ envLabel }}</span>
        </p>
      </div>
      <button class="btn sm" type="button" data-action="reload-extras" :disabled="loading" @click="load()">
        <Icon name="refresh" />{{ t("admin.panel.refresh") }}
      </button>
    </header>

    <p v-if="failure" class="admin-alert" role="alert" data-admin-error>{{ failure }}</p>
    <p v-else-if="loading && !extras" class="admin-loading">{{ t("admin.panel.loading") }}</p>

    <div class="admin-sections">
      <section v-if="sections.pragma" class="admin-sec" data-admin-section="pragma">
        <header class="admin-sec-head">
          <h3>{{ t("admin.pragma.title") }}</h3>
          <span class="muted sm">{{ t("admin.pragma.hint") }}</span>
        </header>
        <p v-if="pragmas?.error" class="admin-alert" role="alert">{{ t("admin.panel.sectionError", { reason: pragmas.error }) }}</p>
        <p v-else-if="!pragmas" class="muted sm">{{ t("admin.pragma.empty") }}</p>
        <template v-else>
          <div class="admin-row" data-pragma="foreign_keys">
            <span class="admin-label">{{ t("admin.pragma.foreignKeys") }}</span>
            <span class="badge" :class="foreignKeys === true ? 'ok' : foreignKeys === false ? 'warn' : 'info'" data-foreign-keys-state>{{ foreignKeyText }}</span>
            <button
              v-if="foreignKeys !== null"
              class="btn sm"
              type="button"
              data-action="toggle-foreign-keys"
              :disabled="busy"
              @click="toggleForeignKeys"
            >{{ foreignKeys ? t("admin.pragma.disableForeignKeys") : t("admin.pragma.enableForeignKeys") }}</button>
          </div>
          <div class="admin-row" data-pragma="journal_mode">
            <span class="admin-label">{{ t("admin.pragma.journalMode") }}</span>
            <Listbox v-model="journal" :options="journalOptions" :aria-label="t('admin.pragma.journalModeAria')" />
            <button class="btn sm" type="button" data-action="preview-journal-mode" :disabled="busy || !journal" @click="saveJournalMode">
              {{ t("admin.panel.preview") }}
            </button>
          </div>
          <div class="admin-row" data-pragma="user_version">
            <span class="admin-label">{{ t("admin.pragma.userVersion") }}</span>
            <input
              v-model="userVersion"
              class="admin-input"
              type="text"
              inputmode="numeric"
              :aria-label="t('admin.pragma.userVersionAria')"
              data-field="user-version"
            />
            <button class="btn sm" type="button" data-action="preview-user-version" :disabled="busy" @click="saveUserVersion">
              {{ t("admin.panel.preview") }}
            </button>
          </div>
        </template>
      </section>

      <section v-if="sections.attached" class="admin-sec" data-admin-section="attached">
        <header class="admin-sec-head">
          <h3>{{ t("admin.attached.title") }}</h3>
          <span class="muted sm">{{ t("admin.attached.hint") }}</span>
        </header>
        <p v-if="extras?.attached.error" class="admin-alert" role="alert">{{ t("admin.panel.sectionError", { reason: extras?.attached.error }) }}</p>
        <table v-if="attached.length" class="admin-table" data-attached-list>
          <thead>
            <tr>
              <th>{{ t("admin.attached.tableName") }}</th>
              <th>{{ t("admin.attached.tableFile") }}</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="item in attached" :key="item.name" :data-attached="item.name">
              <td class="mono">{{ item.name }}</td>
              <td class="mono wrap">{{ item.file || t("admin.attached.main") }}</td>
            </tr>
          </tbody>
        </table>
        <p v-else class="muted sm" data-attached-empty>{{ t("admin.attached.empty") }}</p>
        <div class="admin-form">
          <label class="admin-field">
            <span>{{ t("admin.attached.pathLabel") }}</span>
            <input v-model="attachPath" class="admin-input grow" type="text" :aria-label="t('admin.attached.pathAria')" data-field="attach-path" />
          </label>
          <label class="admin-field">
            <span>{{ t("admin.attached.nameLabel") }}</span>
            <input v-model="attachName" class="admin-input" type="text" :aria-label="t('admin.attached.nameAria')" data-field="attach-name" />
          </label>
          <button class="btn sm" type="button" data-action="attach-database" :disabled="busy" @click="attachDatabase">
            <Icon name="plus" />{{ t("admin.attached.attach") }}
          </button>
        </div>
      </section>

      <section v-if="sections.maintenance" class="admin-sec" data-admin-section="maintenance">
        <header class="admin-sec-head">
          <h3>{{ t("admin.maintenance.title") }}</h3>
          <span class="muted sm">{{ t("admin.maintenance.hint") }}</span>
        </header>
        <div class="admin-form">
          <button class="btn sm" type="button" data-action="vacuum" :disabled="busy" @click="vacuum">{{ t("admin.maintenance.vacuum") }}</button>
          <label class="admin-field">
            <span>{{ t("admin.maintenance.reindexLabel") }}</span>
            <input v-model="reindexTarget" class="admin-input" type="text" :aria-label="t('admin.maintenance.reindexAria')" data-field="reindex-target" />
          </label>
          <button class="btn sm" type="button" data-action="reindex" :disabled="busy" @click="reindex">{{ t("admin.maintenance.reindex") }}</button>
        </div>
      </section>

      <section v-if="sections.extensions" class="admin-sec" data-admin-section="extensions">
        <header class="admin-sec-head">
          <h3>{{ t("admin.extensions.title") }}</h3>
          <span class="muted sm">{{ t("admin.extensions.hint") }}</span>
        </header>
        <p v-if="extras?.extensions.error" class="admin-alert" role="alert">{{ t("admin.panel.sectionError", { reason: extras?.extensions.error }) }}</p>
        <table v-if="extensions.length" class="admin-table" data-extension-list>
          <thead>
            <tr>
              <th>{{ t("admin.extensions.name") }}</th>
              <th>{{ t("admin.extensions.version") }}</th>
              <th>{{ t("admin.extensions.comment") }}</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="item in extensions" :key="item.name" :data-extension="item.name">
              <td class="mono">{{ item.name }}</td>
              <td>
                <span class="badge" :class="item.installedVersion ? 'ok' : 'info'" data-extension-state>
                  {{ item.installedVersion ? t("admin.extensions.installed") : t("admin.extensions.pending") }}
                </span>
                <span class="mono">{{ item.installedVersion ?? t("admin.extensions.defaultVersion", { version: item.defaultVersion }) }}</span>
              </td>
              <td class="wrap">{{ item.comment }}</td>
            </tr>
          </tbody>
        </table>
        <p v-else class="muted sm" data-extension-empty>{{ t("admin.extensions.empty") }}</p>
      </section>

      <section v-if="sections.sequences" class="admin-sec" data-admin-section="sequences">
        <header class="admin-sec-head">
          <h3>{{ t("admin.sequences.title") }}</h3>
          <span class="muted sm">{{ t("admin.sequences.hint") }}</span>
        </header>
        <p v-if="extras?.sequences.error" class="admin-alert" role="alert">{{ t("admin.panel.sectionError", { reason: extras?.sequences.error }) }}</p>
        <table v-if="sequences.length" class="admin-table" data-sequence-list>
          <thead>
            <tr>
              <th>{{ t("admin.sequences.name") }}</th>
              <th>{{ t("admin.sequences.dataType") }}</th>
              <th>{{ t("admin.sequences.increment") }}</th>
              <th>{{ t("admin.sequences.minValue") }}</th>
              <th>{{ t("admin.sequences.maxValue") }}</th>
              <th>{{ t("admin.sequences.start") }}</th>
              <th>{{ t("admin.sequences.cycle") }}</th>
              <th>{{ t("admin.sequences.lastValue") }}</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="item in sequences" :key="`${item.schema}.${item.name}`" :data-sequence="item.name">
              <td class="mono">{{ item.schema }}.{{ item.name }}</td>
              <td class="mono">{{ item.dataType }}</td>
              <td class="mono">{{ item.increment }}</td>
              <td class="mono">{{ item.minValue }}</td>
              <td class="mono">{{ item.maxValue }}</td>
              <td class="mono">{{ item.startValue }}</td>
              <td>{{ item.cycle ? t("admin.sequences.cycle") : "—" }}</td>
              <td class="mono">{{ item.lastValue ?? "—" }}</td>
              <td>
                <button class="btn sm" type="button" data-action="edit-sequence" :disabled="busy" @click="startEdit(item)">
                  {{ t("admin.sequences.edit") }}
                </button>
              </td>
            </tr>
          </tbody>
        </table>
        <p v-else class="muted sm" data-sequence-empty>{{ t("admin.sequences.empty") }}</p>
        <div v-if="editing" class="admin-form" data-sequence-editor>
          <strong class="mono">{{ editing.schema }}.{{ editing.name }}</strong>
          <label class="admin-field">
            <span>{{ t("admin.sequences.increment") }}</span>
            <input v-model="draft.increment" class="admin-input" type="text" inputmode="numeric" :aria-label="t('admin.sequences.incrementAria', { name: editing.name })" data-field="sequence-increment" />
          </label>
          <label class="admin-field">
            <span>{{ t("admin.sequences.minValue") }}</span>
            <input v-model="draft.minValue" class="admin-input" type="text" inputmode="numeric" :aria-label="t('admin.sequences.minValueAria', { name: editing.name })" data-field="sequence-min" />
          </label>
          <label class="admin-field">
            <span>{{ t("admin.sequences.maxValue") }}</span>
            <input v-model="draft.maxValue" class="admin-input" type="text" inputmode="numeric" :aria-label="t('admin.sequences.maxValueAria', { name: editing.name })" data-field="sequence-max" />
          </label>
          <label class="admin-field">
            <span>{{ t("admin.sequences.start") }}</span>
            <input v-model="draft.start" class="admin-input" type="text" inputmode="numeric" :aria-label="t('admin.sequences.startAria', { name: editing.name })" data-field="sequence-start" />
          </label>
          <label class="admin-check">
            <input v-model="draft.cycle" type="checkbox" :aria-label="t('admin.sequences.cycleAria', { name: editing.name })" data-field="sequence-cycle" />
            <span>{{ t("admin.sequences.cycle") }}</span>
          </label>
          <button class="btn primary sm" type="button" data-action="preview-sequence" :disabled="busy" @click="previewSequence">
            {{ t("admin.sequences.preview") }}
          </button>
          <button class="btn sm" type="button" data-action="cancel-sequence" @click="cancelEdit">{{ t("admin.sequences.cancelEdit") }}</button>
        </div>
      </section>

      <section v-if="sections.events" class="admin-sec" data-admin-section="events">
        <header class="admin-sec-head">
          <h3>{{ t("admin.events.title") }}</h3>
          <span class="muted sm">{{ t("admin.events.hint") }}</span>
        </header>
        <p v-if="extras?.events.error" class="admin-alert" role="alert">{{ t("admin.panel.sectionError", { reason: extras?.events.error }) }}</p>
        <table v-if="events.length" class="admin-table" data-event-list>
          <thead>
            <tr>
              <th>{{ t("admin.events.name") }}</th>
              <th>{{ t("admin.events.status") }}</th>
              <th>{{ t("admin.events.schedule") }}</th>
              <th>{{ t("admin.events.onCompletion") }}</th>
              <th>{{ t("admin.events.lastExecuted") }}</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="item in events" :key="`${item.database}.${item.name}`" :data-event="item.name">
              <td class="mono">{{ item.name }}</td>
              <td><span class="badge" :class="item.status.toLowerCase() === 'enabled' ? 'ok' : 'warn'" data-event-state>{{ item.status }}</span></td>
              <td class="mono wrap">{{ item.schedule }}</td>
              <td class="mono">{{ item.onCompletion }}</td>
              <td class="mono">{{ item.lastExecuted ?? "—" }}</td>
              <td>
                <button
                  v-if="item.status.toLowerCase() === 'enabled'"
                  class="btn sm"
                  type="button"
                  data-action="disable-event"
                  :disabled="busy"
                  @click="toggleEvent(item.name, false)"
                >{{ t("admin.events.disable") }}</button>
                <button
                  v-else
                  class="btn sm"
                  type="button"
                  data-action="enable-event"
                  :disabled="busy"
                  @click="toggleEvent(item.name, true)"
                >{{ t("admin.events.enable") }}</button>
              </td>
            </tr>
          </tbody>
        </table>
        <p v-else class="muted sm" data-event-empty>{{ t("admin.events.empty") }}</p>
      </section>

      <section v-if="sections.partitions" class="admin-sec" data-admin-section="partitions">
        <header class="admin-sec-head">
          <h3>{{ t("admin.partitions.title") }}</h3>
          <span class="muted sm">{{ t("admin.partitions.hint") }}</span>
        </header>
        <p v-if="extras?.partitions.error" class="admin-alert" role="alert">{{ t("admin.panel.sectionError", { reason: extras?.partitions.error }) }}</p>
        <div v-if="partitions.length" class="admin-cards" data-partition-list>
          <article v-for="item in partitions" :key="`${item.database}.${item.table}`" class="admin-card" :data-partition-table="item.table">
            <header>
              <strong class="mono">{{ item.table }}</strong>
              <span class="muted sm">{{ t("admin.partitions.method") }}: {{ item.method }}</span>
              <span class="muted sm">{{ t("admin.partitions.count") }}: {{ item.partitions.length }}</span>
            </header>
            <p class="mono wrap">{{ t("admin.partitions.expression") }}: {{ item.expression }}</p>
            <table class="admin-table">
              <thead>
                <tr>
                  <th>{{ t("admin.partitions.partition") }}</th>
                  <th>{{ t("admin.partitions.description") }}</th>
                  <th>{{ t("admin.partitions.rows") }}</th>
                </tr>
              </thead>
              <tbody>
                <tr v-for="part in item.partitions" :key="part.name">
                  <td class="mono">{{ part.name }}</td>
                  <td class="mono wrap">{{ part.description }}</td>
                  <td class="mono">{{ part.rows ?? "—" }}</td>
                </tr>
              </tbody>
            </table>
          </article>
        </div>
        <p v-else class="muted sm" data-partition-empty>{{ t("admin.partitions.empty") }}</p>
      </section>
    </div>

    <ChangePreviewDialog
      v-if="plan"
      :plan="plan"
      :receipt="receipt"
      :busy="busy"
      :engine="String(engine)"
      :connection-name="connectionName"
      :env-label="envLabel"
      :env-tone="envTone"
      @cancel="dismiss"
      @confirm="confirmRun"
    />
  </section>
</template>

<style scoped>
.admin-panel { display: flex; flex-direction: column; gap: var(--sp-3); height: 100%; min-height: 0; overflow: auto; padding: var(--sp-4); }
.admin-bar { display: flex; align-items: center; gap: var(--sp-3); }
.admin-icon {
  display: grid;
  place-items: center;
  width: 28px;
  height: 28px;
  border-radius: var(--r-sm);
  background: var(--accent-soft);
  color: var(--accent-text);
  flex-shrink: 0;
}
.admin-icon .i { width: 15px; height: 15px; }
.admin-titles { flex: 1; min-width: 0; }
.admin-titles h2 { margin: 0; font-size: var(--fs-md); font-weight: 600; }
.admin-titles p { margin: 2px 0 0; font-size: 12px; color: var(--text-3); }
.admin-sections { display: flex; flex-direction: column; gap: var(--sp-3); }
.admin-sec {
  display: flex;
  flex-direction: column;
  gap: var(--sp-2);
  padding: var(--sp-3);
  border: 1px solid var(--line);
  border-radius: var(--r-md);
  background: var(--surface-2, var(--surface));
}
.admin-sec-head { display: flex; align-items: baseline; gap: var(--sp-2); flex-wrap: wrap; }
.admin-sec-head h3 { margin: 0; font-size: var(--fs-sm, 13px); font-weight: 600; }
.admin-row { display: flex; align-items: center; gap: var(--sp-2); flex-wrap: wrap; }
.admin-label { min-width: 160px; font-size: 12px; color: var(--text-2); }
.admin-input {
  padding: 3px 8px;
  border: 1px solid var(--line);
  border-radius: var(--r-sm);
  background: var(--surface);
  color: var(--text);
  font: inherit;
  font-size: 12px;
  min-width: 84px;
}
.admin-input.grow { flex: 1; min-width: 220px; }
.admin-form { display: flex; align-items: flex-end; gap: var(--sp-2); flex-wrap: wrap; }
.admin-field { display: flex; flex-direction: column; gap: 2px; font-size: 11px; color: var(--text-3); }
.admin-check { display: flex; align-items: center; gap: 4px; font-size: 12px; color: var(--text-2); }
.admin-table { width: 100%; border-collapse: collapse; font-size: 12px; }
.admin-table th { text-align: left; font-weight: 500; color: var(--text-3); padding: 3px 6px; border-bottom: 1px solid var(--line); }
.admin-table td { padding: 3px 6px; border-bottom: 1px solid var(--line); vertical-align: top; }
.admin-table td.wrap, .admin-table th.wrap { white-space: normal; }
.admin-cards { display: grid; gap: var(--sp-2); }
.admin-card { display: flex; flex-direction: column; gap: 4px; padding: var(--sp-2); border: 1px dashed var(--line); border-radius: var(--r-sm); }
.admin-card header { display: flex; align-items: center; gap: var(--sp-2); }
.admin-card p { margin: 0; font-size: 12px; color: var(--text-3); }
.admin-alert { margin: 0; padding: var(--sp-2); border-radius: var(--r-sm); background: color-mix(in srgb, var(--danger, #c0392b) 12%, transparent); color: var(--danger-text, var(--text)); font-size: 12px; }
.admin-loading { margin: 0; font-size: 12px; color: var(--text-3); }
</style>
