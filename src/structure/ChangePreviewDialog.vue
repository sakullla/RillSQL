<script setup lang="ts">
import { tr as t } from "../i18n";
import Icon from "../shell/Icon.vue";
import { computed, ref, watch } from "vue";
import { riskLevelLabel, statusLabel, transactionSummary } from "./logic";
import type { StructurePlan, StructureReceipt } from "./types";

const props = defineProps<{ plan: StructurePlan; receipt: StructureReceipt | null; busy: boolean; expectName?: string; engine?: string; connectionName?: string; envLabel?: string; envTone?: string }>();
const emit = defineEmits<{ cancel: []; confirm: [] }>();
const typed = ref("");
const copyNote = ref("");
const detailsOpen = ref(false);

const blocked = computed(() => props.plan.blocked.length > 0 || (props.receipt?.blocked.length ?? 0) > 0);
const nameOk = computed(() => !props.expectName || typed.value === props.expectName);
const receiptBad = computed(() => (props.receipt?.steps ?? []).some((step) => step.status === "failed" || step.status === "unknown"));
const steps = computed(() => props.receipt?.steps ?? props.plan.steps);
const txSummary = computed(() => transactionSummary(props.engine ?? "", props.plan.rebuild));

/* 失败场景自动展开与失败直接相关的部分（R3）。 */
watch(
  receiptBad,
  (bad) => {
    if (bad) detailsOpen.value = true;
  },
  { immediate: true },
);

async function copySql() {
  const sql = steps.value.map((step) => step.sql).join("\n");
  try {
    await navigator.clipboard.writeText(sql);
    copyNote.value = t("structure.change.copied");
  } catch {
    copyNote.value = t("structure.change.copyFailed");
  }
}
</script>

<template>
  <div class="overlay" role="presentation" @click.self="emit('cancel')">
    <div class="sheet" role="dialog" aria-modal="true" :aria-label="t('structure.change.dialogLabel')">
      <header>
        <span class="sheet-icon" :class="{ danger: !receipt && plan.risks.some((risk) => risk.level === 'danger') }"><Icon :name="receipt ? 'terminal' : 'eye'" /></span>
        <h2>{{ receipt ? t("structure.change.resultTitle") : t("structure.change.confirmTitle") }}</h2>
        <span class="grow"></span>
      </header>
      <div class="body">
        <p class="pv-target">{{ t("structure.change.target") }}<template v-if="connectionName">{{ connectionName }} / </template>{{ plan.target }} <span v-if="envLabel" class="env xs" :class="envTone && envTone !== 'none' ? `env-${envTone}` : undefined">{{ envLabel }}</span></p>

        <section v-if="!receipt" class="pv-sec">
          <h3 class="pv-h">{{ t("structure.change.willHappen") }}</h3>
          <ol v-if="plan.steps.length" class="pv-steps">
            <li v-for="(step, index) in plan.steps" :key="index">{{ step.label }}</li>
          </ol>
          <p v-for="(item, index) in plan.blocked" :key="`block-${index}`" class="struct-alert" role="alert">{{ item }}</p>
          <p v-if="!plan.steps.length && !plan.blocked.length" class="muted">{{ t("structure.change.noChanges") }}</p>
        </section>

        <section v-if="plan.risks.length" class="pv-sec">
          <h3 class="pv-h">{{ t("structure.change.attention") }}</h3>
          <p v-for="(risk, index) in plan.risks" :key="`risk-${index}`" class="pv-risk" :class="risk.level">
            <span class="badge" :class="risk.level === 'danger' ? 'err' : risk.level === 'warn' ? 'warn' : 'info'">{{ riskLevelLabel(risk.level) }}</span>
            <span>{{ risk.text }}</span>
          </p>
        </section>

        <p class="pv-tx"><span class="badge info">{{ t("structure.change.txMode") }}</span><span>{{ txSummary }}</span></p>

        <label v-if="expectName && !receipt" class="pv-confirm">
          {{ t("structure.change.dropConfirm", { name: expectName }) }}
          <input v-model="typed" type="text" :aria-label="t('structure.change.confirmNameAria')" :placeholder="t('structure.change.confirmNamePlaceholder')" />
        </label>

        <p v-for="(item, index) in receipt?.blocked ?? []" :key="`done-${index}`" class="struct-alert" role="alert">{{ item }}</p>

        <section v-if="receipt" class="pv-sec">
          <h3 class="pv-h">{{ t("structure.change.stepResults") }}</h3>
          <div v-for="(step, index) in receipt.steps" :key="`r-${index}`" class="receipt-row">
            <span
              class="badge"
              :class="step.status === 'ok' ? 'ok' : step.status === 'failed' || step.status === 'unknown' ? 'err' : 'warn'"
            >{{ statusLabel(step.status) }}</span>
            <span>{{ step.label }}<template v-if="step.message"> — {{ step.message }}</template></span>
          </div>
        </section>

        <details class="disclosure" :open="detailsOpen">
          <summary @click.prevent="detailsOpen = !detailsOpen">{{ t("structure.change.details") }} <span class="muted">{{ t("structure.change.detailsHint") }}</span></summary>
          <div v-if="detailsOpen" class="disclosure-body">
            <p class="muted">{{ receipt?.rule || plan.rule }}</p>
            <section v-for="(step, index) in steps" :key="`sql-${index}`" class="pv-sql">
              <strong>{{ index + 1 }}. {{ step.label }}</strong>
              <pre>{{ step.sql }}</pre>
            </section>
            <div>
              <button class="btn sm" type="button" @click="copySql">{{ t("structure.change.copyAllSql") }}</button>
              <span v-if="copyNote" class="muted"> {{ copyNote }}</span>
            </div>
          </div>
        </details>
      </div>
      <footer>
        <span class="grow"></span>
        <button class="btn" type="button" @click="emit('cancel')">{{ receipt ? t("structure.change.close") : t("structure.change.cancel") }}</button>
        <button v-if="!receipt" class="btn" :class="expectName ? 'danger' : 'primary'" type="button" :disabled="blocked || busy || !nameOk" @click="emit('confirm')">{{ t("structure.change.confirmExecute") }}</button>
      </footer>
    </div>
  </div>
</template>
