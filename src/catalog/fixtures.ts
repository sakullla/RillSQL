import type { CatalogConnection, CatalogObject, PageRequest, PageResult } from "./types";

function table(name: string, estimatedRows: number | null = null): CatalogObject {
  return { name, kind: "table", estimatedRows, stale: false };
}

function view(name: string): CatalogObject {
  return { name, kind: "view", estimatedRows: null, stale: false };
}

export function auditTables(from: number, count: number): CatalogObject[] {
  return Array.from({ length: count }, (_, index) => table(`audit_${String(from + index).padStart(4, "0")}`, 40));
}

export function catalogFixture(): CatalogConnection[] {
  const archive = auditTables(0, 120);
  return [
    {
      id: "atlas",
      name: "Atlas",
      engine: "postgres",
      env: "dev",
      envLabel: "开发",
      envTone: "dev",
      status: "connected",
      defaultDatabase: "atlas_dev",
      foreignKeys: null,
      error: null,
      databases: [
        {
          name: "atlas_dev",
          error: null,
          schemas: [
            {
              name: "public",
              error: null,
              total: 3,
              objects: [table("customers", 2480), table("orders", 6320), table("categories", 12)],
            },
            {
              name: "analytics",
              error: null,
              total: 2,
              objects: [table("customers", 2480), table("daily_metrics", 365)],
            },
            {
              name: "restricted",
              error: "没有权限读取 restricted 里的对象（permission denied）",
              total: 0,
              objects: [],
            },
            {
              name: "archive",
              error: null,
              total: archive.length,
              objects: archive,
            },
          ],
        },
        {
          name: "atlas_audit",
          error: null,
          schemas: [{ name: "public", error: null, total: 1, objects: [table("audit_log", 2)] }],
        },
      ],
    },
    {
      id: "shop",
      name: "Shop",
      engine: "mysql",
      env: "test",
      envLabel: "测试",
      envTone: "test",
      status: "connected",
      defaultDatabase: "shop_staging",
      foreignKeys: null,
      error: null,
      databases: [
        {
          name: "shop_staging",
          error: null,
          schemas: [{ name: "", error: null, total: 2, objects: [table("products", 96), table("orders", 420)] }],
        },
      ],
    },
    {
      id: "ledger",
      name: "Ledger",
      engine: "mariadb",
      env: "dev",
      envLabel: "开发",
      envTone: "dev",
      status: "connected",
      defaultDatabase: "ledger",
      foreignKeys: null,
      error: null,
      databases: [
        {
          name: "ledger",
          error: null,
          schemas: [{ name: "", error: null, total: 1, objects: [table("entries", 18)] }],
        },
      ],
    },
    {
      id: "notes",
      name: "本地笔记",
      engine: "sqlite",
      env: "local",
      envLabel: "本地",
      envTone: "local",
      status: "connected",
      defaultDatabase: null,
      foreignKeys: false,
      error: null,
      databases: [
        {
          name: "main",
          error: null,
          schemas: [{ name: "", error: null, total: 2, objects: [table("notes", 64), view("v_note_summary")] }],
        },
      ],
    },
    {
      id: "analytics",
      name: "Analytics",
      engine: "mariadb",
      env: "prod",
      envLabel: "生产",
      envTone: "prod",
      status: "disconnected",
      defaultDatabase: "analytics",
      foreignKeys: null,
      error: null,
      databases: [
        {
          name: "analytics",
          error: null,
          schemas: [{ name: "", error: null, total: 1, objects: [table("events", 9000)] }],
        },
      ],
    },
  ];
}

export function pageArchive(request: PageRequest): PageResult {
  if (request.schema !== "archive") {
    return { objects: [], total: 0, error: null };
  }
  const all = auditTables(0, 120);
  return {
    objects: all.slice(request.offset, request.offset + request.limit),
    total: all.length,
    error: null,
  };
}

export function pagedCatalog(pageSize = 100): CatalogConnection[] {
  return catalogFixture().map((connection) => ({
    ...connection,
    databases: connection.databases.map((database) => ({
      ...database,
      schemas: database.schemas.map((schema) => {
        if (schema.objects.length <= pageSize) return schema;
        return { ...schema, objects: schema.objects.slice(0, pageSize), total: schema.total };
      }),
    })),
  }));
}

export interface RefreshFixture {
  hide(connectionId: string, database: string, schema: string, objectName: string): void;
  snapshot(): CatalogConnection[];
}

export function createRefreshFixture(base: CatalogConnection[] = catalogFixture()): RefreshFixture {
  const hidden = new Set<string>();
  return {
    hide(connectionId, database, schema, objectName) {
      hidden.add(`${connectionId}/${database}/${schema}/${objectName}`);
    },
    snapshot() {
      return base.map((connection) => ({
        ...connection,
        databases: connection.databases.map((database) => ({
          ...database,
          schemas: database.schemas.map((schema) => {
            if (schema.error) return { ...schema, objects: [], total: 0 };
            const objects = schema.objects.filter((object) => !hidden.has(`${connection.id}/${database.name}/${schema.name}/${object.name}`));
            return { ...schema, objects, total: objects.length, error: null };
          }),
        })),
      }));
    },
  };
}
