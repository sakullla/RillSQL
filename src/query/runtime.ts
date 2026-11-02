import { shallowReactive } from "vue";
import { tr } from "../i18n";
import { loadSettings } from "../shell/settings";
import { queryCancel, queryPoll, queryRelease, queryStart, type RunDto, type StartRequest } from "./api";

export interface RunSnapshot extends RunDto {
  tabId: string;
  pollError: string;
}

const runs = shallowReactive<Record<string, RunSnapshot>>({});
const timers = new Map<string, ReturnType<typeof setInterval>>();
const closeAfter = new Set<string>();
const pendingPolls = new Map<string, string>();
const settledWaiters = new Map<string, Set<{ runId: string; resolve: () => void }>>();
let closer: ((id: string) => void) | null = null;
let epoch = 0;
let startingQueries = 0;

export function hasRunningQueries(): boolean {
  return startingQueries > 0 || Object.values(runs).some(isLive);
}

export function tabRun(tabId: string): RunSnapshot | undefined {
  return runs[tabId];
}

export function attachCloser(fn: (id: string) => void) {
  closer = fn;
}

export function connectionBusy(connectionId: string, exceptTabId: string): boolean {
  return Object.entries(runs).some(([tabId, run]) => tabId !== exceptTabId && run.connectionId === connectionId && isLive(run));
}

export async function startTabRun(input: StartRequest & { tabId: string }): Promise<void> {
  if (connectionBusy(input.connectionId, input.tabId)) {
    throw new Error(tr("query.runtime.busyConnection"));
  }
  const token = epoch;
  const { tabId, ...request } = input;
  // 语句超时是本机设置，每次发起时读取；0 表示不限制，字段整体缺省。
  const timeoutSec = loadSettings().queryTimeoutSec;
  startingQueries += 1;
  let dto: RunDto;
  try {
    dto = await queryStart(timeoutSec > 0 ? { ...request, timeoutMs: timeoutSec * 1000 } : request);
  } finally {
    startingQueries -= 1;
  }
  if (token !== epoch) return;
  adoptRun(tabId, dto);
  if (isLive(dto)) watchRun(tabId);
  else stopTimer(tabId);
}

/** 从同一次结果流再读下一块。运行已经结束时什么都不做。 */
export async function continueTabRun(tabId: string): Promise<void> {
  const current = runs[tabId];
  if (!current || !isLive(current)) return;
  const token = epoch;
  const runId = current.id;
  try {
    const next = await queryPoll(runId, true);
    if (token !== epoch || runs[tabId]?.id !== runId) return;
    apply(tabId, next);
  } catch (err) {
    if (token !== epoch) return;
    const run = runs[tabId];
    if (run?.id === runId && isLive(run)) run.pollError = errorText(err);
  }
  settle(tabId);
}

/** 等到这次运行不再占用连接。已经结束时立刻返回。 */
export function whenSettled(tabId: string): Promise<void> {
  const current = runs[tabId];
  if (!current || !isLive(current)) return Promise.resolve();
  const runId = current.id;
  return new Promise((resolve) => {
    let waiters = settledWaiters.get(tabId);
    if (!waiters) settledWaiters.set(tabId, waiters = new Set());
    waiters.add({ runId, resolve });
  });
}

function resolveSettled(tabId: string) {
  const waiters = settledWaiters.get(tabId);
  if (!waiters) return;
  const current = runs[tabId];
  for (const waiter of waiters) {
    if (current?.id === waiter.runId && isLive(current)) continue;
    waiters.delete(waiter);
    waiter.resolve();
  }
  if (!waiters.size) settledWaiters.delete(tabId);
}

export async function requestCancel(tabId: string): Promise<void> {
  const current = runs[tabId];
  if (!current || !isLive(current)) return;
  if (current.status === "cancel-requested" && current.cancel === "requested") return;
  current.status = "cancel-requested";
  current.cancel = "requested";
  if (!current.cancelMessage) {
    current.cancelMessage = tr(current.viaSsh ? "query.runtime.cancelViaSsh" : "query.runtime.cancelWait");
  }
  const token = epoch;
  const runId = current.id;
  try {
    const next = await queryCancel(runId);
    if (token !== epoch || runs[tabId]?.id !== runId) return;
    apply(tabId, next);
  } catch (err) {
    if (token !== epoch) return;
    const run = runs[tabId];
    if (!run || run.id !== runId || !isLive(run)) return;
    run.cancel = "failed";
    run.status = "running";
    run.cancelMessage = errorText(err);
  }
  settle(tabId);
}

export function guardClose(tabId: string): boolean {
  const run = runs[tabId];
  if (!run || !isLive(run)) {
    delete runs[tabId];
    stopTimer(tabId);
    resolveSettled(tabId);
    return true;
  }
  closeAfter.add(tabId);
  void requestCancel(tabId);
  return false;
}

export function adoptRun(tabId: string, dto: RunDto) {
  runs[tabId] = shallowReactive({ ...dto, tabId, pollError: "" });
  releaseFinished(dto);
  resolveSettled(tabId);
}

function releaseFinished(dto: RunDto) {
  // UI 持有最终结果；后端无需再为历史查询保留一份结果集。释放失败不改变查询结果。
  if (!isLive(dto)) void queryRelease(dto.id).catch(() => undefined);
}

export function resetQueryRuntime() {
  epoch += 1;
  for (const timer of timers.values()) clearInterval(timer);
  timers.clear();
  pendingPolls.clear();
  closeAfter.clear();
  closer = null;
  for (const id of Object.keys(runs)) delete runs[id];
  for (const tabId of settledWaiters.keys()) resolveSettled(tabId);
}

function watchRun(tabId: string) {
  stopTimer(tabId);
  const timer = setInterval(() => {
    void refresh(tabId);
  }, 400);
  timers.set(tabId, timer);
}

async function refresh(tabId: string) {
  if (pendingPolls.has(tabId)) return;
  const current = runs[tabId];
  if (!current) {
    stopTimer(tabId);
    return;
  }
  const token = epoch;
  const runId = current.id;
  try {
    pendingPolls.set(tabId, runId);
    const next = await queryPoll(runId);
    if (token !== epoch || runs[tabId]?.id !== runId) return;
    apply(tabId, next);
  } catch (err) {
    if (token !== epoch) return;
    const run = runs[tabId];
    if (run?.id === runId && isLive(run)) run.pollError = errorText(err);
  } finally {
    if (pendingPolls.get(tabId) === runId) pendingPolls.delete(tabId);
  }
  settle(tabId);
}

function apply(tabId: string, next: RunDto) {
  const current = runs[tabId];
  // 并发的继续读取/取消/轮询可能晚到，不允许把终态退回执行中。
  if (current?.id === next.id && !isLive(current)) return;
  if (current && current.id === next.id && cancelRank(next.cancel) < cancelRank(current.cancel) && isLive(next)) {
    runs[tabId] = shallowReactive({
      ...next,
      tabId,
      pollError: "",
      cancel: current.cancel,
      cancelMessage: current.cancelMessage,
      status: current.status,
    });
    return;
  }
  runs[tabId] = shallowReactive({ ...next, tabId, pollError: "" });
  releaseFinished(next);
  resolveSettled(tabId);
}

function settle(tabId: string) {
  const run = runs[tabId];
  if (!run) return;
  if (!isLive(run)) stopTimer(tabId);
  if (!closeAfter.has(tabId)) return;
  if (run.cancel === "confirmed" || run.status === "cancelled") {
    closeAfter.delete(tabId);
    stopTimer(tabId);
    closer?.(tabId);
  }
}

function stopTimer(tabId: string) {
  const timer = timers.get(tabId);
  if (!timer) return;
  clearInterval(timer);
  timers.delete(tabId);
}

function isLive(run: { status: string }): boolean {
  return run.status === "running" || run.status === "cancel-requested";
}

function cancelRank(cancel: string): number {
  if (cancel === "confirmed") return 3;
  if (cancel === "failed") return 2;
  if (cancel === "requested") return 1;
  return 0;
}

export function errorText(err: unknown): string {
  if (typeof err === "string") return err;
  if (err instanceof Error) return err.message;
  return tr("query.runtime.executeFailed");
}
