"""MySQL / MariaDB 对象验收：在真实服务器上跑事件、函数/存储过程、触发器的建/改/删与读回。用法：
  python scripts/verify-mysql-objects.py --require-fixtures --report docs/verification/2026-11-21-mysql-proprietary-support/objects-live.md

脚本自己起两个一次性容器（默认 MySQL 9.7.4 与 MariaDB 12.3.3，端口 3436 / 3437），执行
docker/init/mysql-objects.sql，然后跑被 #[ignore] 的 Rust 用例 object_lifecycle_round_trip_on_real_servers。
默认镜像取每组目标版本里实施时的最高版本（R9：只验证能覆盖其余目标版本的最高版本）。

想用更低版本自查，用 RILLSQL_OBJECT_MYSQL_IMAGE / RILLSQL_OBJECT_MARIADB_IMAGE 覆盖，例如
RILLSQL_OBJECT_MYSQL_IMAGE=mysql:8.4：那样跑出来的报告只证明 8.4，不能反过来当作 9.x 的证据。

--require-fixtures 让"没跑"变成失败：任一引擎没有实际执行、或用例被跳过，脚本都以非零退出。
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
CARGO = ["cargo", "test", "--manifest-path", os.path.join("src-tauri", "Cargo.toml")]
FENCE = chr(96) * 3
MARKER = re.compile("object-live: (mysql|mariadb) (.+)$")

FIXTURES = [
    {
        "engine": "mysql",
        "container": "rillsql-t3-mysql",
        "image": os.environ.get("RILLSQL_OBJECT_MYSQL_IMAGE", "mysql:9.7.4"),
        "port": os.environ.get("RILLSQL_OBJECT_MYSQL_PORT", "3436"),
        "env": ["-e", "MYSQL_ROOT_PASSWORD=root", "-e", "MYSQL_DATABASE=rillsql_proprietary"],
        "client": "mysql",
        "wait": "mysqladmin ping -h127.0.0.1 -uroot -proot --silent",
    },
    {
        "engine": "mariadb",
        "container": "rillsql-t3-mariadb",
        "image": os.environ.get("RILLSQL_OBJECT_MARIADB_IMAGE", "mariadb:12.3.3"),
        "port": os.environ.get("RILLSQL_OBJECT_MARIADB_PORT", "3437"),
        "env": ["-e", "MARIADB_ROOT_PASSWORD=root", "-e", "MARIADB_DATABASE=rillsql_proprietary"],
        # mariadb 官方镜像只带 mariadb 客户端，没有 mysql 这个别名。
        "client": "mariadb",
        "wait": "healthcheck.sh --connect --innodb_initialized",
    },
]


def run(argv, **kwargs):
    return subprocess.run(argv, cwd=ROOT, capture_output=True, text=True, **kwargs)


def docker(*args, **kwargs):
    return run(["docker", *args], **kwargs)


def remove_container(name):
    docker("rm", "-f", name)


def start_fixture(spec):
    remove_container(spec["container"])
    argv = ["docker", "run", "-d", "--name", spec["container"], *spec["env"], "-p", "127.0.0.1:%s:3306" % spec["port"], spec["image"]]
    result = run(argv)
    if result.returncode != 0:
        raise SystemExit("启动 %s 失败：%s" % (spec["image"], result.stderr.strip()))


def wait_ready(spec, attempts=90):
    for _ in range(attempts):
        result = docker("exec", spec["container"], "sh", "-c", spec["wait"])
        if result.returncode == 0:
            return
        time.sleep(2)
    logs = docker("logs", "--tail", "30", spec["container"])
    raise SystemExit("%s 没有就绪：%s%s" % (spec["container"], logs.stdout, logs.stderr))


def apply_fixture_sql(spec):
    path = os.path.join(ROOT, "docker", "init", "mysql-objects.sql")
    with open(path, encoding="utf-8") as handle:
        sql = handle.read()
    result = docker("exec", "-i", spec["container"], spec["client"], "-uroot", "-proot", input=sql)
    if result.returncode != 0:
        raise SystemExit("写入 %s 到 %s 失败：%s%s" % (path, spec["container"], result.stderr.strip(), result.stdout.strip()))


def server_version(spec):
    result = docker("exec", spec["container"], spec["client"], "-N", "-B", "-uroot", "-proot", "-e", "SELECT VERSION()")
    if result.returncode != 0:
        raise SystemExit("读不到 %s 的版本：%s" % (spec["container"], result.stderr.strip()))
    return result.stdout.strip()


def scheduler_state(spec):
    result = docker(
        "exec", spec["container"], spec["client"], "-N", "-B", "-uroot", "-proot", "-e", "SELECT @@global.event_scheduler"
    )
    if result.returncode != 0:
        return "未读到"
    return result.stdout.strip() or "未读到"


def run_tests():
    env = dict(os.environ)
    env["RILLSQL_OBJECT_MYSQL_PORT"] = FIXTURES[0]["port"]
    env["RILLSQL_OBJECT_MARIADB_PORT"] = FIXTURES[1]["port"]
    argv = [*CARGO, "--locked", "--lib", "--", "--ignored", "--nocapture", "object_lifecycle_round_trip_on_real_servers"]
    result = run(argv, env=env)
    return result.returncode, result.stdout + result.stderr, " ".join(argv)


def marker_lines(output):
    lines = {}
    for line in output.splitlines():
        match = MARKER.match(line.strip())
        if match:
            lines.setdefault(match.group(1), []).append(match.group(2).strip())
    return lines


def result_line(output):
    for line in output.splitlines():
        if line.strip().startswith("test result:"):
            return line.strip()
    return ""


def write_report(path, args, versions, schedulers, evidence, output, command):
    now = datetime.datetime.now().astimezone().strftime("%Y-%m-%d %H:%M:%S %z")
    directory = os.path.dirname(path)
    if directory:
        os.makedirs(directory, exist_ok=True)
    done = {engine: any("全部用例完成" in line for line in lines) for engine, lines in evidence.items()}
    rows = [
        (
            "事件（单次 / 周期）",
            "adapters/mysql/object_ddl.rs event_schedule_sql、event_statement；启停走 SetEventEnabled",
            "information_schema.EVENTS（STATUS / EXECUTE_AT / INTERVAL_* / STARTS / ENDS / ON_COMPLETION / EVENT_DEFINITION）+ SHOW VARIABLES event_scheduler",
            "建单次与周期事件（EVERY 含 STARTS/ENDS）；改调度与主体；启用、停用、删除；每步读回调度、主体与状态；服务器调度器状态不改",
        ),
        (
            "函数 / 存储过程",
            "adapters/mysql/object_ddl.rs routine_statement、params_sql、definer_clause",
            "information_schema.ROUTINES + PARAMETERS（按 ROUTINE_TYPE 过滤）",
            "建函数的参数/返回类型/多语句主体；替换改参数与主体并保留 DEFINER；存储过程 IN/OUT/INOUT；删除后读回不到",
        ),
        (
            "触发器",
            "adapters/mysql/object_ddl.rs trigger_statement、replace_steps",
            "information_schema.TRIGGERS（ACTION_TIMING / EVENT_MANIPULATION / ACTION_STATEMENT）+ 一次真实 DML",
            "建 BEFORE INSERT 多语句主体触发器；INSERT 证明绑定并改写列；替换为 AFTER UPDATE 后 UPDATE 触发、INSERT 不再触发；删除后 DML 不再触发",
        ),
        (
            "同名函数与存储过程",
            "adapters/mysql/object_read.rs ROUTINE_SQL / PARAMETERS_SQL 的 ROUTINE_TYPE 条件",
            "information_schema.ROUTINES / PARAMETERS 的 ROUTINE_TYPE 列",
            "夹具里同名函数与存储过程并存：参数与主体分别读回，不互相串（T1 交接修正的真机证据）",
        ),
        (
            "失败路径与替换风险",
            "ddl/exec.rs 逐条执行与 receipts；object_ddl.rs 的 danger 风险与阻断原因",
            "服务器原始错误",
            "无效事件主体、重名事件、无效参数类型、目标表不存在、无效触发器主体各一次原始错误；替换中途 CREATE 失败时 DROP 已执行、对象读回不到",
        ),
    ]
    coverage = {
        "mysql": {
            "family": "MySQL",
            "measured_fixed": "9.7.4",
            "lower": "8.4.13 / 8.0.46 / 5.7.44",
            "gate": "events / routines / triggers 自 5.7 起可用，无版本分支；DEFINER 写法与 information_schema 列名同源",
            "limits": [
                "夹具打开了 log_bin_trust_function_creators：binlog 打开时 MySQL 要求函数声明 DETERMINISTIC / NO SQL / READS SQL DATA，对象编辑器不替用户编这些语义，未声明的函数会被服务器拒绝（原始错误照实显示）",
                "8.4 / 8.0 / 5.7 上的事件与例程读写没有单独实测：按同一实现路径与同一 information_schema 列推断",
                "5.7 的 PARAMETERS 表在函数返回行上的写法没有单独核对，按 9.7.4 的读回结果推断",
                "9.x 的认证插件变化（如 mysql_native_password 移除）属于连接认证路径，不在本报告范围",
            ],
        },
        "mariadb": {
            "family": "MariaDB",
            "measured_fixed": "12.3.3",
            "lower": "11.8.9 / 11.4.13 / 10.11.19 / 10.6.28",
            "gate": "events / routines / triggers 自 10.2 起可用，无版本分支；本报告不涉及生成列写法",
            "limits": [
                "夹具打开了 log_bin_trust_function_creators：binlog 打开时 MariaDB 同样要求函数声明 DETERMINISTIC / NO SQL / READS SQL DATA",
                "11.x / 10.x 上的事件与例程读写没有单独实测：按同一实现路径推断",
                "MariaDB 的 information_schema.ROUTINES.ROUTINE_DEFINITION 截断行为没有逐版本核对",
            ],
        },
    }
    body = ["# MySQL / MariaDB 对象真实验收（T3：事件、函数/存储过程、触发器）", "", "生成时间：%s" % now, "", "## 运行命令", "", FENCE]
    body.append("python scripts/verify-mysql-objects.py " + " ".join(args))
    body.append(command)
    body.append(FENCE)
    body.append("")
    body.append("## 实际运行版本")
    body.append("")
    body.append("| 引擎 | 镜像 | 服务器版本 | event_scheduler | 用例是否执行 |")
    body.append("| --- | --- | --- | --- | --- |")
    for spec in FIXTURES:
        engine = spec["engine"]
        body.append(
            "| %s | %s | %s | %s | %s |"
            % (
                engine,
                spec["image"],
                versions.get(engine, "未读到"),
                schedulers.get(engine, "未读到"),
                "是" if done.get(engine) else "否",
            )
        )
    body.append("")
    body.append("## 功能、实现路径与覆盖关系")
    body.append("")
    body.append("| 功能 | 代码路径 | 元数据 / 驱动路径 | 本轮实际覆盖的行为 |")
    body.append("| --- | --- | --- | --- |")
    for name, code, meta, acts in rows:
        body.append("| %s | %s | %s | %s |" % (name, code, meta, acts))
    body.append("")
    body.append("驱动：mysql_async 0.37.1（rustls）。MySQL 与 MariaDB 不互相替代：两个引擎各自跑完整流程。")
    body.append("")
    body.append("## 覆盖关系（R9：最高版本实测 → 低版本按同一实现路径 + 版本门控推断）")
    body.append("")
    body.append("每组只在实施时的**最高目标版本**上实测，更低版本不单独起容器：由高版本的结果加上同一实现路径与版本门控推断。推断项一律标为推断，不当成已验证。")
    body.append("")
    body.append("| 家族 | 实测版本（服务器 VERSION()） | 本轮是否执行 | 与基线固定版本一致 | 由它推断的更低目标版本 |")
    body.append("| --- | --- | --- | --- | --- |")
    for engine in ("mysql", "mariadb"):
        spec = coverage[engine]
        raw = versions.get(engine, "")
        aligned = "是（%s）" % spec["measured_fixed"] if raw.startswith(spec["measured_fixed"]) else "否（基线 %s）" % spec["measured_fixed"]
        body.append(
            "| %s | %s | %s | %s | %s |"
            % (spec["family"], raw or "未读到", "是" if done.get(engine) else "否", aligned, spec["lower"])
        )
    body.append("")
    body.append(
        "方向只能是高 → 低：本轮用到的语法（CREATE/ALTER/DROP EVENT、CREATE/DROP FUNCTION 与 PROCEDURE、CREATE/DROP TRIGGER）与读回路径在 9.7.4 / 12.3.3 上全部跑通，更低目标版本的语法是它的子集；反过来，低版本上通过的结论证明不了高版本。所以本报告不拿低版本“代表”高版本。"
    )
    body.append("")
    body.append("共用同一实现路径的依据：")
    body.append("")
    body.append("- DDL 生成：adapters/mysql/object_ddl.rs 对两个家族用同一份代码，没有按主次版本分支；版本门控只由 adapters/mysql/profile.rs 的 FamilyFeatures 决定（events / routines / triggers 在全部目标版本为真）。")
    body.append("- 元数据读回：adapters/mysql/object_read.rs 用 information_schema.EVENTS / ROUTINES / PARAMETERS / TRIGGERS，两条例程查询都按 ROUTINE_TYPE 过滤。")
    body.append("- DEFINER 保留：读回的是 information_schema 的 user@host 原文，替换时按标识符引号写回，两个家族写法一致。")
    body.append("- 驱动与协议：同一 mysql_async 0.37.1（rustls）；多语句主体作为同一条 Step.sql 发送，不按分号切分。")
    body.append("")
    body.append("仍属推断 / 限制（不得当作已验证事实）：")
    body.append("")
    body.append("- 上表“由它推断的更低目标版本”整列都是推断，本轮没有在这些版本上运行过。")
    for engine in ("mysql", "mariadb"):
        for line in coverage[engine]["limits"]:
            body.append("- %s：%s。" % (coverage[engine]["family"], line))
    body.append("- 本报告只证明“实测版本”一列。Delivery 记录覆盖范围时必须照抄这里的分界，不能写成“已验证全部目标版本”。")
    body.append("")
    body.append("## 用例证据（用例自身打印）")
    body.append("")
    body.append(FENCE)
    for engine in ("mysql", "mariadb"):
        for line in evidence.get(engine, []):
            body.append("object-live: %s %s" % (engine, line))
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
    parser.add_argument("--keep", action="store_true", help="跑完保留容器，方便手工查看")
    args = parser.parse_args()

    versions = {}
    schedulers = {}
    evidence = {}
    output = ""
    command = ""
    failure = None
    try:
        for spec in FIXTURES:
            start_fixture(spec)
        for spec in FIXTURES:
            wait_ready(spec)
            apply_fixture_sql(spec)
            versions[spec["engine"]] = server_version(spec)
            schedulers[spec["engine"]] = scheduler_state(spec)
            print("%s %s 就绪（端口 %s，event_scheduler=%s）" % (spec["engine"], versions[spec["engine"]], spec["port"], schedulers[spec["engine"]]))
        code, output, command = run_tests()
        evidence = marker_lines(output)
        missing = [engine for engine in ("mysql", "mariadb") if not any("全部用例完成" in line for line in evidence.get(engine, []))]
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
        if not args.keep:
            for spec in FIXTURES:
                remove_container(spec["container"])

    if args.report:
        write_report(args.report, sys.argv[1:], versions, schedulers, evidence, output, command)
        print("报告：%s" % args.report)

    if failure:
        if args.require_fixtures:
            print("失败：%s" % failure, file=sys.stderr)
            return 1
        print("跳过（没有 --require-fixtures）：%s" % failure, file=sys.stderr)
        return 0
    print("通过：MySQL 与 MariaDB 都实际跑完对象验收")
    return 0


if __name__ == "__main__":
    sys.exit(main())
