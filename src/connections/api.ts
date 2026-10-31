import { invoke } from "@tauri-apps/api/core";
import type { ConnectionDraft, EnvTag, EnvTone, SavedConnection, SecretInput } from "./model";

export interface StageView {
  kind: string;
  label: string;
  status: "ok" | "err" | "skipped" | "wait";
  message: string;
  /** 稳定错误码（T6-A2/R6）：失败/等待阶段携带，前端按码取字典文案。 */
  code?: string | null;
}

export interface TestReport {
  status: "ok" | "err" | "wait";
  failStage?: string | null;
  hint?: string | null;
  summary?: string | null;
  fingerprint?: string | null;
  algorithm?: string | null;
  stages: StageView[];
  /** 失败/等待阶段的稳定错误码（T6-A2/R6）：呈现处先查码取字典文案。 */
  code?: string | null;
}

export interface SaveOutcome {
  connection: SavedConnection;
  note?: string | null;
}

export interface DeleteOutcome {
  connection: SavedConnection;
  databaseFileRemoved: boolean;
}

export interface OpenOutcome {
  report: TestReport;
  connected: boolean;
  foreignKeys?: boolean | null;
}

export interface EnvTagInput {
  id?: string;
  label: string;
  tone: EnvTone;
}

export interface ExportOutcome {
  count: number;
}

/** RillSQL 导出文件解密后的逐条解析结果：引擎不受支持的条目进 skipped 报告。 */
export interface ImportFileOutcome {
  connections: SavedConnection[];
  skipped: { name: string; reason: string }[];
}

export interface ConnectionApi {
  list(): Promise<SavedConnection[]>;
  listTags(): Promise<EnvTag[]>;
  saveTag(input: EnvTagInput): Promise<EnvTag>;
  deleteTag(id: string): Promise<void>;
  save(draft: ConnectionDraft, secrets: SecretInput): Promise<SaveOutcome>;
  remove(id: string): Promise<DeleteOutcome>;
  test(id: string, secrets: SecretInput): Promise<TestReport>;
  open(id: string, secrets: SecretInput): Promise<OpenOutcome>;
  disconnect(id: string): Promise<void>;
  confirmHostKey(id: string, fingerprint: string): Promise<void>;
  forgetHostKey(id: string): Promise<void>;
  export(ids: string[] | null, passphrase: string, path: string): Promise<ExportOutcome>;
  importFile(path: string, passphrase: string): Promise<ImportFileOutcome>;
}

function emptyToNull(value: string): string | null {
  const trimmed = value.trim();
  return trimmed ? trimmed : null;
}

export const tauriConnectionApi: ConnectionApi = {
  list: () => invoke<SavedConnection[]>("connections_list"),
  listTags: () => invoke<EnvTag[]>("env_tags_list"),
  saveTag: (input) => invoke<EnvTag>("env_tags_save", { id: input.id ?? null, label: input.label, tone: input.tone }),
  deleteTag: (id) => invoke<void>("env_tags_delete", { id }),
  save: (draft, secrets) =>
    invoke<SaveOutcome>("connections_save", {
      draft,
      password: emptyToNull(secrets.password),
      sshPassword: emptyToNull(secrets.sshPassword),
      sshPassphrase: emptyToNull(secrets.passphrase),
    }),
  remove: (id) => invoke<DeleteOutcome>("connections_delete", { id }),
  test: (id, secrets) =>
    invoke<TestReport>("connections_test", {
      id,
      password: emptyToNull(secrets.password),
      sshPassword: emptyToNull(secrets.sshPassword),
      sshPassphrase: emptyToNull(secrets.passphrase),
    }),
  open: (id, secrets) =>
    invoke<OpenOutcome>("connections_open", {
      id,
      password: emptyToNull(secrets.password),
      sshPassword: emptyToNull(secrets.sshPassword),
      sshPassphrase: emptyToNull(secrets.passphrase),
    }),
  disconnect: (id) => invoke<void>("connections_disconnect", { id }),
  confirmHostKey: (id, fingerprint) => invoke<void>("connections_confirm_host_key", { id, fingerprint }),
  forgetHostKey: (id) => invoke<void>("connections_forget_host_key", { id }),
  export: (ids, passphrase, path) =>
    invoke<ExportOutcome>("connections_export", { ids, passphrase: emptyToNull(passphrase), path }),
  importFile: (path, passphrase) =>
    invoke<ImportFileOutcome>("connections_import_file", { path, passphrase: emptyToNull(passphrase) }),
};
