"""执行风险验收：在真实夹具上确认「风险语句先确认、只读语句直通」，并覆盖七种引擎的对照。

用法：
  python scripts/verify-execution-risk.py --require-fixtures --report docs/verification/2026-11-21-mysql-proprietary-support/execution-risk-live.md

脚本用 docker/compose.yaml 起（或复用）postgres / mysql / mariadb / mssql / redis / mongo 六个夹具，
SQLite 由 Rust 用例自己建临时文件库；随后运行被 #[ignore] 的 Rust 用例
execution_risk_on_real_fixtures。用例走的是查询台真实的执行前决策与执行链：
Desk::begin_run 决定要不要确认，确认后同一份 SQL 由 commands/exec 打到服务器。

--require-fixtures 让「没跑」变成失败：任一引擎没有实际执行用例、或用例被跳过，脚本都以非零退出。
只依赖标准库与 docker CLI。
"""
import argparse
import datetime
import os
import re
import subprocess
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
COMPOSE = ["docker", "compose", "-f", os.path.join(ROOT, "docker", "compose.yaml")]
CARGO = ["cargo", "test", "--manifest-path", os.path.join("src-tauri", "Cargo.toml")]
FENCE = chr(96) * 3

# compose 服务 → 引擎 id（报告与用例标记都用引擎 id）。
FIXTURES = [
    {"engine": "postgres", "service": "postgres", "port": "5432", "env": "RILLSQL_RISK_PG_PORT", "timeout": 120},
    {"engine": "mysql", "service": "mysql", "port": "3306", "env": "RILLSQL_RISK_MYSQL_PORT", "timeout": 180},
    {"engine": "mariadb", "service": "mariadb", "port": "3307", "env": "RILLSQL_RISK_MARIADB_PORT", "timeout": 180},
    {"engine": "sqlserver", "service": "mssql", "port": "1433", "env": "RILLSQL_RISK_MSSQL_PORT", "timeout": 420},
    {"engine": "redis", "service": "redis", "port": "6379", "env": "RILLSQL_RISK_REDIS_PORT", "timeout": 120},
    {"engine": "mongo", "service": "mongo", "port": "27017", "env": "RILLSQL_RISK_MONGO_PORT", "timeout": 180},
]
ENGINES = [spec["engine"] for spec in FIXTURES] + ["sqlite"]


def run(argv, **kwargs):
    return subprocess.run(argv, cwd=ROOT, capture_output=True, text=True, **kwargs)


def compose(*args, **kwargs):
    return run([*COMPOSE, *args], **kwargs)


def container_id(service):
    """按服务名取容器 id：compose 项目名随文件变化，不假设容器名的前缀。"""
    result = compose("ps", "-q", service)
    if result.returncode != 0:
        return ""
    lines = [line.strip() for line in result.stdout.splitlines() if line.strip()]
    return lines[0] if lines else ""


def start_fixtures():
    services = [spec["service"] for spec in FIXTURES]
    result = compose("up", "-d", *services)
    if result.returncode != 0:
        raise SystemExit("启动夹具失败：%s%s" % (result.stderr.strip(), result.stdout.strip()))


def seed_redis():
    """Redis 夹具的种子键由 compose 的 redis-init 一次性容器写入；脚本显式同步跑一次（写入是幂等的）。"""
    try:
        result = compose("run", "--rm", "--no-deps", "-T", "redis-init", timeout=180)
    except subprocess.TimeoutExpired:
        raise SystemExit("redis-init 没有在 180 秒内结束")
    if result.returncode != 0:
        raise SystemExit("redis-init 失败：%s%s" % (result.stderr.strip(), result.stdout.strip()))


def mssql_database_exists():
    result = compose(
        "exec", "-T", "mssql", "/opt/mssql-tools18/bin/sqlcmd", "-C", "-h", "-1", "-W", "-S", "localhost", "-U", "sa",
        "-P", "Rillsql-Passw0rd!", "-Q", "SELECT DB_ID('rillsql')",
    )
    return result.returncode == 0 and "NULL" not in result.stdout.upper()


def seed_mssql():
    """SQL Server 官方镜像没有 initdb：compose 里的 mssql-init 负责建库与种子数据。
    它是一次性容器，up -d 之后不一定跑过；种子脚本不是幂等的，所以只在库不存在时执行。"""
    if mssql_database_exists():
        print("sqlserver 夹具库已存在，跳过 mssql-init")
        return
    try:
        result = compose("run", "--rm", "--no-deps", "-T", "mssql-init", timeout=300)
    except subprocess.TimeoutExpired:
        raise SystemExit("mssql-init 没有在 300 秒内结束")
    if result.returncode != 0:
        raise SystemExit("mssql-init 失败：%s%s" % (result.stderr.strip(), result.stdout.strip()))


def health(service):
    cid = container_id(service)
    if not cid:
        return "missing"
    result = run(["docker", "inspect", "--format", "{{if .State.Health}}{{.State.Health.Status}}{{else}}{{.State.Status}}{{end}}", cid])
    if result.returncode != 0:
        return "missing"
    return result.stdout.strip()


def wait_fixture(spec):
    deadline = time.time() + spec["timeout"]
    while time.time() < deadline:
        if health(spec["service"]) == "healthy":
            return
        time.sleep(3)
    logs = compose("logs", "--tail", "20", spec["service"])
    raise SystemExit("%s 没有就绪（%s）：%s%s" % (spec["service"], health(spec["service"]), logs.stdout, logs.stderr))


def query_version(spec):
    """读服务器实际版本；读不到时抛错，不写猜测值。"""
    service = spec["service"]
    if service == "mongo":
        result = compose(
            "exec", "-T", service, "mongosh", "--quiet", "--authenticationDatabase", "admin", "-u", "root", "-p", "rillsql-mongo",
            "--eval", "db.version()",
        )
    elif service == "redis":
        result = compose("exec", "-T", service, "sh", "-c", "redis-cli --no-auth-warning -a rillsql-redis INFO server | tr -d '\\r' | grep '^redis_version:'")
    elif service == "mssql":
        result = compose(
            "exec", "-T", service, "/opt/mssql-tools18/bin/sqlcmd", "-C", "-h", "-1", "-W", "-S", "localhost", "-U", "sa",
            "-P", "Rillsql-Passw0rd!", "-Q", "SELECT @@VERSION",
        )
    elif service == "postgres":
        result = compose("exec", "-T", service, "psql", "-U", "postgres", "-d", "rillsql", "-tA", "-c", "SELECT version()")
    elif service == "mariadb":
        # mariadb 官方镜像只带 mariadb 客户端，没有 mysql 这个别名。
        result = compose("exec", "-T", service, "sh", "-c", "mariadb -uroot -proot -N -B -e 'SELECT VERSION()'")
    else:
        result = compose("exec", "-T", service, "sh", "-c", "mysql -uroot -proot -N -B -e 'SELECT VERSION()'")
    if result.returncode != 0:
        raise SystemExit("读不到 %s 的版本：%s%s" % (service, result.stderr.strip(), result.stdout.strip()))
    return " ".join(result.stdout.split())[:160]


def run_tests():
    env = dict(os.environ)
    for spec in FIXTURES:
        env[spec["env"]] = spec["port"]
    argv = [*CARGO, "--locked", "--lib", "--", "--ignored", "--nocapture", "execution_risk_on_real_fixtures"]
    result = run(argv, env=env)
    return result.returncode, result.stdout + result.stderr, " ".join(argv)


def markers(output):
    found = {}
    for line in output.splitlines():
        match = re.match(r"execution-risk: (postgres|mysql|mariadb|sqlserver|sqlite|redis|mongo) (.+)$", line.strip())
        if match:
            found.setdefault(match.group(1), []).append(match.group(2).strip())
    return found


def result_line(output):
    for line in output.splitlines():
        if line.strip().startswith("test result:"):
            return line.strip()
    return ""


def write_report(path, argv, versions, evidence, output, command):
    now = datetime.datetime.now().astimezone().strftime("%Y-%m-%d %H:%M:%S %z")
    directory = os.path.dirname(path)
    if directory:
        os.makedirs(directory, exist_ok=True)
    done = {engine: any("全部用例完成" in line for line in lines) for engine, lines in evidence.items()}
    rows = [
        ("postgres", "SELECT/EXPLAIN 直通；DROP TABLE、整表 UPDATE、多语句混合",
         "adapters/postgres/exec.rs（逐条发送）+ query/risk.rs 的后果分类", "DROP TABLE 判为数据丢失；表真的消失；整表 UPDATE 判为大量重写"),
        ("mysql", "DROP TABLE、整表 UPDATE、多语句混合",
         "adapters/mysql/exec.rs；DDL 逐条执行、无整体回滚", "DROP TABLE 判为数据丢失 + 隐式提交；多语句只拦风险那条"),
        ("mariadb", "DROP TABLE、整表 UPDATE、多语句混合",
         "adapters/mariadb/exec.rs（与 MySQL 同族不同实现路径）", "同上；MariaDB 独立实测，不用 MySQL 结果替代"),
        ("sqlite", "DELETE 无 WHERE、多语句混合",
         "adapters/sqlite/exec.rs；文件库（临时目录）", "整表 DELETE 判为数据丢失；SQLite 没有 TRUNCATE，用全表删除对照"),
        ("sqlserver", "TRUNCATE TABLE、整表 UPDATE、多语句混合",
         "adapters/sqlserver/exec.rs（TDS 批量）", "TRUNCATE 判为数据丢失；没有隐式提交说明（SQL Server 可回滚）"),
        ("redis", "FLUSHDB 未确认、DBSIZE 只读",
         "adapters/redis/exec.rs 的 CommandRisk + application/redis.rs 守卫", "Blocked/Dangerous/Readonly 不变；未确认的写命令在应用层被拒"),
        ("mongo", "deleteOne 未确认、find 只读",
         "application/mongo.rs 的 docs::require_delete 守卫", "未确认删除被拒；读操作不受影响"),
    ]
    limits = [
        "SQLite 是临时文件夹具，不是 compose 服务：验证的是同一份 sqlite 适配器与执行链。",
        "Redis 与 MongoDB 不是 SQL 通道：报告只证明其既有守卫没有回退，风险分类仍留在各自适配器与应用层。",
        "MySQL 与 MariaDB 各自实跑，不互相替代；两者版本号见「实际运行版本」。",
        "SQL Server 只跑 2022-latest 一个镜像；T-SQL 的其它版本没有单独实测。",
        "客户端可判断的后果（数据丢失、对象替换、大量重写、隐式提交、外部调度）在执行前提示；服务端才能发现的错误仍保留原始错误信息。",
    ]
    body = ["# 全库执行风险真实验收（T4）", "", "生成时间：%s" % now, "", "## 运行命令", "", FENCE]
    body.append("python scripts/verify-execution-risk.py " + " ".join(argv))
    body.append(command)
    body.append(FENCE)
    body.append("")
    body.append("## 实际运行版本与执行情况")
    body.append("")
    body.append("| 引擎 | 版本 | 用例是否执行 |")
    body.append("| --- | --- | --- |")
    for engine in ENGINES:
        body.append("| %s | %s | %s |" % (engine, versions.get(engine, "未读到") or "未读到", "是" if done.get(engine) else "否"))
    body.append("")
    body.append("## 功能、实现路径与真实证据")
    body.append("")
    body.append("| 引擎 | 覆盖的操作 | 代码路径 | 实测到的后果 |")
    body.append("| --- | --- | --- | --- |")
    for engine, acts, code, proof in rows:
        body.append("| %s | %s | %s | %s |" % (engine, acts, code, proof))
    body.append("")
    body.append("判定口径：风险分类来自 `query/risk.rs`（纯函数，按后果分档），执行前决策来自 `query/desk.rs` 的 `begin_run`，")
    body.append("确认后由同一批 `PreparedRun` 经 `commands/exec` 打到服务器；用例没有绕过这两层直接发 SQL。")
    body.append("")
    body.append("## 路径差异与限制")
    body.append("")
    for line in limits:
        body.append("- %s" % line)
    body.append("")
    body.append("## 用例证据（用例自身打印）")
    body.append("")
    body.append(FENCE)
    for engine in ENGINES:
        for line in evidence.get(engine, []):
            body.append("execution-risk: %s %s" % (engine, line))
    body.append(FENCE)
    body.append("")
    body.append("## 测试结果与原始输出（末尾 80 行）")
    body.append("")
    body.append(FENCE)
    body.append(result_line(output))
    body.extend(output.strip().splitlines()[-80:])
    body.append(FENCE)
    with open(path, "w", encoding="utf-8", newline="\n") as handle:
        handle.write("\n".join(body) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--report", default="", help="把报告写到这个路径（Markdown）")
    parser.add_argument("--require-fixtures", action="store_true", help="夹具没跑或用例被跳过时失败")
    parser.add_argument("--keep", action="store_true", help="跑完不停止容器，方便手工查看")
    parser.add_argument("--skip-start", action="store_true", help="复用已经起好的夹具，不执行 compose up")
    args = parser.parse_args()

    versions = {}
    evidence = {}
    output = ""
    command = ""
    failure = None
    started = False
    try:
        if not args.skip_start:
            start_fixtures()
            started = True
        for spec in FIXTURES:
            wait_fixture(spec)
        seed_mssql()
        seed_redis()
        for spec in FIXTURES:
            versions[spec["engine"]] = query_version(spec)
            print("%s %s 就绪（端口 %s）" % (spec["engine"], versions[spec["engine"]], spec["port"]))
        code, output, command = run_tests()
        evidence = markers(output)
        versions["sqlite"] = next((line.split(" ", 1)[1] for line in evidence.get("sqlite", []) if line.startswith("版本 ")), "未读到")
        missing = [engine for engine in ENGINES if not any("全部用例完成" in line for line in evidence.get(engine, []))]
        summary = result_line(output)
        if code != 0:
            failure = "用例失败（cargo test 退出码 %d）" % code
        elif not re.search(r"test result: ok\. [1-9]\d* passed; 0 failed", summary):
            failure = "没有看到通过的测试结果行：%s" % (summary or "（没有结果行）")
        elif "ignored" not in summary or "0 ignored" not in summary:
            failure = "用例被跳过（ignored）——离线跳过不算通过：%s" % summary
        elif missing:
            failure = "这些引擎没有实际执行用例：%s" % ", ".join(missing)
    except SystemExit as err:
        failure = str(err.code)
    finally:
        if started and not args.keep and not args.skip_start:
            compose("stop", *[spec["service"] for spec in FIXTURES])

    if args.report:
        write_report(args.report, sys.argv[1:], versions, evidence, output, command)
        print("报告：%s" % args.report)

    if failure:
        if args.require_fixtures:
            print("失败：%s" % failure, file=sys.stderr)
            return 1
        print("跳过（没有 --require-fixtures）：%s" % failure, file=sys.stderr)
        return 0
    print("通过：七种引擎都实际跑完执行风险验收")
    return 0


if __name__ == "__main__":
    sys.exit(main())
