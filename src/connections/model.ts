import { ENGINES, engineMeta, type Engine } from "../engines";
import { i18n, tr } from "../i18n";

export { ENGINES, engineMeta };
export type { Engine };

export type EnvTone = "dev" | "test" | "prod" | "local";
export type TlsMode = "off" | "require" | "verify";
export type CaMode = "system" | "file";
export type SshAuth = "password" | "key";

export interface ConnectionDraft {
  id?: string;
  name: string;
  engine: Engine;
  env: string;
  host: string;
  port: string;
  database: string;
  username: string;
  filePath: string;
  /** MongoDB 专用：replicaSet 名（可选，空串=不设置）。 */
  replicaSet: string;
  tlsMode: TlsMode;
  caMode: CaMode;
  caFile: string;
  clientCert: string;
  clientKey: string;
  sshOn: boolean;
  sshHost: string;
  sshPort: string;
  sshUser: string;
  sshAuth: SshAuth;
  sshKey: string;
  savePassword: boolean;
}

export interface SecretInput {
  password: string;
  sshPassword: string;
  passphrase: string;
}

export interface TlsSettings {
  mode: TlsMode;
  serverName: string;
  caMode: CaMode;
  caFile?: string | null;
  clientCert?: string | null;
  clientKey?: string | null;
}

export interface SshSettings {
  host: string;
  port: number;
  username: string;
  auth: SshAuth;
  keyPath?: string | null;
}

export interface SavedConnection {
  id: string;
  name: string;
  engine: Engine;
  env: string;
  host?: string | null;
  port?: number | null;
  database?: string | null;
  username?: string | null;
  filePath?: string | null;
  /** MongoDB 专用：replicaSet 连接串参数；其余引擎恒为 null。 */
  replicaSet?: string | null;
  tls?: TlsSettings | null;
  ssh?: SshSettings | null;
  passwordSaved: boolean;
  sshPasswordSaved: boolean;
  sshHostKey?: string | null;
}

export const ENV_NONE = "none";

export interface EnvTag {
  id: string;
  label: string;
  tone: EnvTone;
  position: number;
}

export const TONES: { id: EnvTone; label: string }[] = [
  { id: "dev", label: "connections.tones.dev" },
  { id: "test", label: "connections.tones.test" },
  { id: "prod", label: "connections.tones.prod" },
  { id: "local", label: "connections.tones.local" },
];

export function presentEnv(env: string, tags: readonly EnvTag[]): { label: string; tone: EnvTone | "none" } {
  if (!env || env === ENV_NONE) return { label: tr("connections.page.noTag"), tone: "none" };
  const tag = tags.find((item) => item.id === env);
  if (!tag) return { label: tr("connections.page.noTag"), tone: "none" };
  return { label: tag.label, tone: tag.tone };
}

export function blankDraft(engine: Engine): ConnectionDraft {
  const meta = engineMeta(engine);
  return {
    name: "",
    engine,
    env: engine === "sqlite" || engine === "redis" || engine === "mongodb" ? "local" : "dev",
    host: "",
    port: meta.port,
    // Redis 的“数据库”字段是库索引（SELECT），默认 0 号库。
    database: engine === "redis" ? "0" : "",
    username: "",
    filePath: "",
    replicaSet: "",
    // Redis/MongoDB 自部署多为明文端口；默认不加密，避免夹具/本机直连被默认 Verify 挡住。
    tlsMode: engine === "sqlite" || engine === "redis" || engine === "mongodb" ? "off" : "verify",
    caMode: "system",
    caFile: "",
    clientCert: "",
    clientKey: "",
    sshOn: false,
    sshHost: "",
    sshPort: "22",
    sshUser: "",
    sshAuth: "key",
    sshKey: "",
    savePassword: engine !== "sqlite",
  };
}

/** Redis 的“数据库”字段语义是库索引：空串或非负整数（服务器端通常 0–15）。 */
export function validateRedisDbIndex(value: string): string | null {
  const trimmed = value.trim();
  if (!trimmed) return null;
  if (!/^\d+$/.test(trimmed)) return tr("connections.validate.redisDbIndex");
  return null;
}

/** MongoDB 的主机字段可以填整条 mongodb:// 或 mongodb+srv:// 连接串；此时端口/认证库等以串为准。 */
export function isMongoUri(host: string): boolean {
  const lowered = host.trim().toLowerCase();
  return lowered.startsWith("mongodb://") || lowered.startsWith("mongodb+srv://");
}

export function draftFromSaved(saved: SavedConnection): ConnectionDraft {
  return {
    id: saved.id,
    name: saved.name,
    engine: saved.engine,
    env: saved.env,
    host: saved.host ?? "",
    port: saved.port ? String(saved.port) : "",
    database: saved.database ?? "",
    username: saved.username ?? "",
    filePath: saved.filePath ?? "",
    replicaSet: saved.replicaSet ?? "",
    tlsMode: saved.engine === "sqlite" ? "off" : (saved.tls?.mode ?? "verify"),
    caMode: saved.tls?.caMode ?? "system",
    caFile: saved.tls?.caFile ?? "",
    clientCert: saved.tls?.clientCert ?? "",
    clientKey: saved.tls?.clientKey ?? "",
    sshOn: Boolean(saved.ssh),
    sshHost: saved.ssh?.host ?? "",
    sshPort: String(saved.ssh?.port ?? 22),
    sshUser: saved.ssh?.username ?? "",
    sshAuth: saved.ssh?.auth ?? "key",
    sshKey: saved.ssh?.keyPath ?? "",
    savePassword: saved.passwordSaved,
  };
}

export function validate(draft: ConnectionDraft): Record<string, string> {
  const errors: Record<string, string> = {};
  if (draft.engine === "sqlite") {
    if (!draft.filePath.trim()) errors.filePath = tr("connections.validate.chooseFile");
    return errors;
  }
  if (!draft.host.trim()) errors.host = tr("connections.validate.hostRequired");
  // MongoDB 填整条连接串时地址与端口都在串里，端口字段可留空。
  const mongoUri = draft.engine === "mongodb" && isMongoUri(draft.host);
  const port = Number(draft.port);
  if (!mongoUri && (!/^\d+$/.test(draft.port.trim()) || port < 1 || port > 65535)) errors.port = tr("connections.validate.portRange");
  // Redis 的用户名是 ACL 用户名（密码认证可留空）；MongoDB 允许无认证直连（本地开发）。
  if (!draft.username.trim() && draft.engine !== "redis" && draft.engine !== "mongodb") errors.username = tr("connections.validate.userRequired");
  if (draft.engine === "redis") {
    const dbError = validateRedisDbIndex(draft.database);
    if (dbError) errors.database = dbError;
  }
  // MongoDB 整串 URI 与 SSH 隧道互斥（后端保存阶段同样拒绝）：URI 的地址直接进驱动，隧道会被绕过。
  if (draft.engine === "mongodb" && isMongoUri(draft.host) && draft.sshOn) {
    errors.sshHost = tr("connections.validate.mongoUriSsh");
  }
  if (draft.sshOn && !draft.sshHost.trim()) errors.sshHost = tr("connections.validate.sshHostRequired");
  if (draft.sshOn && !draft.sshUser.trim()) errors.sshUser = tr("connections.validate.sshUserRequired");
  const sshPort = Number(draft.sshPort);
  if (draft.sshOn && (!/^\d+$/.test(draft.sshPort.trim()) || sshPort < 1 || sshPort > 65535)) errors.sshPort = tr("connections.validate.portRange");
  if (draft.tlsMode !== "off" && draft.caMode === "file" && !draft.caFile.trim()) errors.caFile = tr("connections.validate.caFileRequired");
  return errors;
}

function nonempty(value: string): string | null {
  const trimmed = value.trim();
  return trimmed ? trimmed : null;
}

/** 普通配置。密码、SSH 密码和私钥口令都不在这个对象里。 */
export function toStoredConfig(draft: ConnectionDraft, id: string): SavedConnection {
  if (draft.engine === "sqlite") {
    const filePath = draft.filePath.trim();
    const fallback = filePath.split(/[/\\]/).pop() || filePath;
    return {
      id,
      name: draft.name.trim() || fallback,
      engine: "sqlite",
      env: draft.env,
      host: null,
      port: null,
      database: null,
      username: null,
      filePath,
      replicaSet: null,
      tls: null,
      ssh: null,
      passwordSaved: false,
      sshPasswordSaved: false,
    };
  }
  const host = draft.host.trim();
  return {
    id,
    name: draft.name.trim() || host,
    engine: draft.engine,
    env: draft.env,
    host,
    port: Number(draft.port),
    database: nonempty(draft.database),
    username: draft.username.trim(),
    filePath: null,
    replicaSet: draft.engine === "mongodb" ? nonempty(draft.replicaSet) : null,
    tls: {
      mode: draft.tlsMode,
      serverName: host,
      caMode: draft.caMode,
      caFile: nonempty(draft.caFile),
      clientCert: nonempty(draft.clientCert),
      clientKey: nonempty(draft.clientKey),
    },
    ssh: draft.sshOn
      ? {
          host: draft.sshHost.trim(),
          port: Number(draft.sshPort) || 22,
          username: draft.sshUser.trim(),
          auth: draft.sshAuth,
          keyPath: nonempty(draft.sshKey),
        }
      : null,
    passwordSaved: false,
    sshPasswordSaved: false,
  };
}

/** 与后端 src-tauri/src/connections/stages.rs 的阶段标签保持一致。 */
export function plannedStageLabels(draft: ConnectionDraft): string[] {
  if (draft.engine === "sqlite") {
    return [tr("connections.stages.sqliteOpen"), tr("connections.stages.sqliteTables"), tr("connections.stages.sqliteForeignKeys")];
  }
  const stages = draft.sshOn
    ? [tr("connections.stages.sshConfirm"), tr("connections.stages.sshLogin"), tr("connections.stages.sshTunnel")]
    : [tr("connections.stages.connectServer")];
  if (draft.tlsMode !== "off") stages.push(draft.tlsMode === "verify" ? tr("connections.stages.tlsVerify") : tr("connections.stages.tlsRequire"));
  stages.push(tr("connections.stages.login"), tr("connections.stages.version"));
  return stages;
}

/**
 * 按 StageView.kind（后端 StageKind 的 snake_case 码）取阶段名（T6-A3）：
 * zh-CN 直接用后端 label 原文（保留 TLS verify 等措辞）；其他 locale 用 connections.stageKinds 字典，
 * 未知 kind 回退后端 label 原文。
 */
export function stageKindLabel(kind: string, fallback: string): string {
  const key = `connections.stageKinds.${kind}`;
  return i18n.global.te(key) ? tr(key as Parameters<typeof tr>[0]) : fallback;
}

/** 按 kind 的成功阶段摘要（T6-A3）：非 zh-CN locale 的 ok 消息映射；未知 kind 回退动态原文。 */
export function stageOkSummary(kind: string, fallback: string): string {
  const key = `connections.stageOk.${kind}`;
  return i18n.global.te(key) ? tr(key as Parameters<typeof tr>[0]) : fallback;
}

export function tlsVerificationCopy(draft: ConnectionDraft): string {
  const host = draft.host.trim() || tr("connections.tlsCopy.hostPending");
  if (draft.sshOn) {
    return tr("connections.tlsCopy.viaSsh", { host });
  }
  return tr("connections.tlsCopy.direct");
}

export function storedConfigHidesSecrets(stored: SavedConnection, secrets: SecretInput): boolean {
  const blob = JSON.stringify(stored);
  if (/"password"\s*:/.test(blob) || /"sshPassword"\s*:/.test(blob) || /"passphrase"\s*:/.test(blob)) return false;
  return [secrets.password, secrets.sshPassword, secrets.passphrase].every((value) => value.length === 0 || !blob.includes(value));
}
