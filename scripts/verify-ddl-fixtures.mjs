import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";

const LOG = "docs/verification/run/docker-ddl.log";
const CONTAINERS = {
  postgres: "rillsql-dev-postgres-1",
  mysql: "rillsql-dev-mysql-1",
  mariadb: "rillsql-dev-mariadb-1",
};

if (process.argv.includes("--docker")) {
  execFileSync("docker", ["compose", "-f", "docker/compose.yaml", "up", "-d", "postgres", "mysql", "mariadb"], { stdio: "inherit" });
}

function run(container, args) {
  const output = execFileSync("docker", ["exec", container, ...args], { encoding: "utf8" });
  return output.trim();
}

function expect(value, expected, label, log) {
  log.push(`${label}: ${value}`);
  if (value !== expected) throw new Error(`${label} 期望 ${expected}，实际 ${value}`);
}

fs.mkdirSync(path.dirname(LOG), { recursive: true });
const started = new Date().toISOString();
const log = [`# docker ddl fixtures @ ${started}`];
try {
  const pg = CONTAINERS.postgres;
  run(pg, ["psql", "-U", "postgres", "-d", "rillsql", "-v", "ON_ERROR_STOP=1", "-c", "DROP SEQUENCE IF EXISTS demo_seq; CREATE SEQUENCE demo_seq START 1;"]);
  run(pg, ["psql", "-U", "postgres", "-d", "rillsql", "-v", "ON_ERROR_STOP=1", "-c", "ALTER SEQUENCE demo_seq INCREMENT BY 5;"]);
  expect(run(pg, ["psql", "-U", "postgres", "-d", "rillsql", "-At", "-c", "SELECT increment_by FROM pg_sequences WHERE sequencename='demo_seq';"]), "5", "postgres.sequence_increment", log);
  run(pg, ["psql", "-U", "postgres", "-d", "rillsql", "-v", "ON_ERROR_STOP=1", "-c", "DROP TYPE IF EXISTS mood; CREATE TYPE mood AS ENUM ('sad','ok'); ALTER TYPE mood ADD VALUE 'happy';"]);
  expect(run(pg, ["psql", "-U", "postgres", "-d", "rillsql", "-At", "-c", "SELECT string_agg(enumlabel, ',' ORDER BY enumsortorder) FROM pg_enum e JOIN pg_type t ON t.oid=e.enumtypid WHERE t.typname='mood';"]), "sad,ok,happy", "postgres.enum_values", log);

  for (const [engine, container] of [["mysql", CONTAINERS.mysql], ["mariadb", CONTAINERS.mariadb]]) {
    const client = engine === "mysql" ? "mysql" : "mariadb";
    run(container, [client, "-urillsql", "-prillsql", "rillsql", "-e", "DROP EVENT IF EXISTS demo_event; CREATE EVENT demo_event ON SCHEDULE EVERY 1 DAY DO SELECT 1; ALTER EVENT demo_event DISABLE;"]);
    expect(run(container, [client, "-urillsql", "-prillsql", "-N", "-e", "SELECT STATUS FROM information_schema.EVENTS WHERE EVENT_NAME='demo_event';"]), "DISABLED", `${engine}.event_status`, log);
  }
  log.push("result: passed");
  console.log("docker ddl fixtures: passed");
} catch (error) {
  log.push(`result: failed - ${error instanceof Error ? error.message : String(error)}`);
  fs.writeFileSync(LOG, log.join("\n") + "\n");
  console.error(error instanceof Error ? error.message : error);
  process.exit(1);
}
fs.writeFileSync(LOG, log.join("\n") + "\n");
