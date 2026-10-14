export type Engine = "mysql" | "mariadb" | "postgres" | "sqlite";
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
  tls?: TlsSettings | null;
  ssh?: SshSettings | null;
  passwordSaved: boolean;
  sshPasswordSaved: boolean;
  sshHostKey?: string | null;
}

export const ENGINES: { id: Engine; label: string; short: string; port: string }[] = [
  { id: "postgres", label: "PostgreSQL", short: "PG", port: "5432" },
  { id: "mysql", label: "MySQL", short: "MY", port: "3306" },
  { id: "mariadb", label: "MariaDB", short: "MA", port: "3306" },
  { id: "sqlite", label: "SQLite", short: "SQL", port: "" },
];

export const ENV_NONE = "none";

export interface EnvTag {
  id: string;
  label: string;
  tone: EnvTone;
  position: number;
}

export const TONES: { id: EnvTone; label: string }[] = [
  { id: "dev", label: "青绿" },
  { id: "test", label: "蓝色" },
  { id: "prod", label: "红色" },
  { id: "local", label: "紫色" },
];

export function engineMeta(engine: Engine) {
  return ENGINES.find((item) => item.id === engine) ?? ENGINES[0];
}

export function presentEnv(env: string, tags: readonly EnvTag[]): { label: string; tone: EnvTone | "none" } {
  if (!env || env === ENV_NONE) return { label: "无标签", tone: "none" };
  const tag = tags.find((item) => item.id === env);
  if (!tag) return { label: "无标签", tone: "none" };
  return { label: tag.label, tone: tag.tone };
}

export function blankDraft(engine: Engine): ConnectionDraft {
  const meta = engineMeta(engine);
  return {
    name: "",
    engine,
    env: engine === "sqlite" ? "local" : "dev",
    host: "",
    port: meta.port,
    database: "",
    username: "",
    filePath: "",
    tlsMode: engine === "sqlite" ? "off" : "verify",
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
    if (!draft.filePath.trim()) errors.filePath = "请选择数据库文件";
    return errors;
  }
  if (!draft.host.trim()) errors.host = "请填写主机";
  const port = Number(draft.port);
  if (!/^\d+$/.test(draft.port.trim()) || port < 1 || port > 65535) errors.port = "端口应为 1–65535";
  if (!draft.username.trim()) errors.username = "请填写用户名";
  if (draft.sshOn && !draft.sshHost.trim()) errors.sshHost = "请填写跳板机主机";
  if (draft.sshOn && !draft.sshUser.trim()) errors.sshUser = "请填写 SSH 用户";
  const sshPort = Number(draft.sshPort);
  if (draft.sshOn && (!/^\d+$/.test(draft.sshPort.trim()) || sshPort < 1 || sshPort > 65535)) errors.sshPort = "端口应为 1–65535";
  if (draft.tlsMode !== "off" && draft.caMode === "file" && !draft.caFile.trim()) errors.caFile = "请填写 CA 证书文件";
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

export function plannedStageLabels(draft: ConnectionDraft): string[] {
  if (draft.engine === "sqlite") return ["打开文件", "读取 schema", "检查 foreign_keys"];
  const stages = draft.sshOn ? ["SSH 主机身份", "SSH 认证", "建立隧道"] : ["网络连接"];
  if (draft.tlsMode !== "off") stages.push(draft.tlsMode === "verify" ? "TLS 握手与证书验证" : "TLS 握手");
  stages.push("数据库认证", "读取服务器版本");
  return stages;
}

export function tlsVerificationCopy(draft: ConnectionDraft): string {
  const host = draft.host.trim() || "（填写主机后显示）";
  if (draft.sshOn) {
    return `证书仍按数据库主机名 ${host} 校验，网络指向 127.0.0.1，不会改用 localhost，也不会关闭验证后重试。`;
  }
  return "直连时，网络地址和证书主机名都是数据库主机。";
}

export function storedConfigHidesSecrets(stored: SavedConnection, secrets: SecretInput): boolean {
  const blob = JSON.stringify(stored);
  if (/"password"\s*:/.test(blob) || /"sshPassword"\s*:/.test(blob) || /"passphrase"\s*:/.test(blob)) return false;
  return [secrets.password, secrets.sshPassword, secrets.passphrase].every((value) => value.length === 0 || !blob.includes(value));
}
