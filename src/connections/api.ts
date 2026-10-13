import { invoke } from "@tauri-apps/api/core";
import type { ConnectionDraft, SavedConnection, SecretInput } from "./model";

export interface StageView {
  kind: string;
  label: string;
  status: "ok" | "err" | "skipped" | "wait";
  message: string;
}

export interface TestReport {
  status: "ok" | "err" | "wait";
  failStage?: string | null;
  hint?: string | null;
  summary?: string | null;
  fingerprint?: string | null;
  algorithm?: string | null;
  stages: StageView[];
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

export interface ConnectionApi {
  list(): Promise<SavedConnection[]>;
  save(draft: ConnectionDraft, secrets: SecretInput): Promise<SaveOutcome>;
  remove(id: string): Promise<DeleteOutcome>;
  test(id: string, secrets: SecretInput): Promise<TestReport>;
  open(id: string, secrets: SecretInput): Promise<OpenOutcome>;
  disconnect(id: string): Promise<void>;
  confirmHostKey(id: string, fingerprint: string): Promise<void>;
  forgetHostKey(id: string): Promise<void>;
}

function emptyToNull(value: string): string | null {
  const trimmed = value.trim();
  return trimmed ? trimmed : null;
}

export const tauriConnectionApi: ConnectionApi = {
  list: () => invoke<SavedConnection[]>("connections_list"),
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
};
