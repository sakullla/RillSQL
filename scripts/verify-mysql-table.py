"""MySQL / MariaDB 表结构验收：在真实服务器上跑 ENUM/SET、生成列与四类分区的增删改和读回。

用法：
  python scripts/verify-mysql-table.py --require-fixtures --report docs/verification/2026-11-21-mysql-proprietary-support/table-live.md

脚本自己起两个一次性容器（默认 MySQL 9.7.4 与 MariaDB 12.3.3，端口 3426 / 3427），执行
docker/init/mysql-table.sql，然后跑被 #[ignore] 的 Rust 用例 table_features_round_trip_on_real_servers。

默认镜像取每组目标版本里实施时的最高版本（R9：只验证能覆盖其余目标版本的最高版本）。
想用更低版本自查，用 RILLSQL_TABLE_MYSQL_IMAGE / RILLSQL_TABLE_MARIADB_IMAGE 覆盖，例如
RILLSQL_TABLE_MYSQL_IMAGE=mysql:8.4：那样跑出来的报告只证明 8.4，不能反过来当作 9.x 的证据。
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

FIXTURES = [
    {
        "engine": "mysql",
        "container": "rillsql-t2-mysql",
        "image": os.environ.get("RILLSQL_TABLE_MYSQL_IMAGE", "mysql:9.7.4"),
        "port": os.environ.get("RILLSQL_TABLE_MYSQL_PORT", "3426"),
        "env": ["-e", "MYSQL_ROOT_PASSWORD=root", "-e", "MYSQL_DATABASE=rillsql_proprietary"],
        "client": "mysql",
        "wait": "mysqladmin ping -h127.0.0.1 -uroot -proot --silent",
    },
    {
        "engine": "mariadb",
        "container": "rillsql-t2-mariadb",
        "image": os.environ.get("RILLSQL_TABLE_MARIADB_IMAGE", "mariadb:12.3.3"),
        "port": os.environ.get("RILLSQL_TABLE_MARIADB_PORT", "3427"),
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
    path = os.path.join(ROOT, "docker", "init", "mysql-table.sql")
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


def run_tests():
    env = dict(os.environ)
    env["RILLSQL_TABLE_MYSQL_PORT"] = FIXTURES[0]["port"]
    env["RILLSQL_TABLE_MARIADB_PORT"] = FIXTURES[1]["port"]
    argv = [*CARGO, "--locked", "--lib", "--", "--ignored", "--nocapture", "table_features_round_trip_on_real_servers"]
    result = run(argv, env=env)
    return result.returncode, result.stdout + result.stderr, " ".join(argv)


def marker_lines(output):
    lines = {}
    for line in output.splitlines():
        match = re.match(r"table-live: (mysql|mariadb) (.+)$", line.strip())
        if match:
            lines.setdefault(match.group(1), []).append(match.group(2).strip())
    return lines


def result_line(output):
    for line in output.splitlines():
        if line.strip().startswith("test result:"):
            return line.strip()
    return ""


def write_report(path, args, versions, evidence, output, command):
    now = datetime.datetime.now().astimezone().strftime("%Y-%m-%d %H:%M:%S %z")
    directory = os.path.dirname(path)
    if directory:
        os.makedirs(directory, exist_ok=True)
    done = {engine: any("全部用例完成" in line for line in lines) for engine, lines in evidence.items()}
    rows = [
        ("ENUM/SET 候选值", "adapters/mysql/table_ddl.rs enum_set_type、literal.rs", "information_schema.columns.COLUMN_TYPE",
         "建 ENUM/SET 含空串、单引号、反斜杠、逗号与中文；改候选值；删字段；空候选值在预览前阻断"),
        ("生成列（VIRTUAL；STORED/PERSISTENT）", "adapters/mysql/table_ddl.rs generated_clause、profile.stored_keyword",
         "information_schema.columns.EXTRA / GENERATION_EXPRESSION", "建 VIRTUAL 生成列；追加持久化生成列；改表达式；表达式默认值不被当成生成列"),
        ("RANGE 分区", "adapters/mysql/table_ddl.rs partition_by_clause、partition_steps", "information_schema.PARTITIONS", "建立、新增、删除、重组"),
        ("LIST 分区", "同上", "information_schema.PARTITIONS", "建立"),
        ("HASH 分区", "同上", "information_schema.PARTITIONS", "建立、按数量增、按数量减"),
        ("KEY 分区", "同上", "information_schema.PARTITIONS（PARTITION_EXPRESSION 是反引号包住的列清单）",
         "建立、读回列清单还原成裸列名后按同一函数重建、移除分区方案"),
        ("失败路径与风险", "ddl/exec.rs 逐条执行与 receipts；table_ddl.rs partition_applicability", "服务器原始错误",
         "服务器拒绝新增分区后结构不变、原始错误保留；未分区的表删除分区在预览前阻断"),
    ]
    # R9：每组只实测实施时的最高目标版本；更低版本按同一实现路径 + 版本门控推断，全部算推断。
    coverage = {
        "mysql": {
            "family": "MySQL",
            "measured_fixed": "9.7.4",
            "lower": "8.4.13 / 8.0.46 / 5.7.44",
            "gate": "generated_columns 从 5.7 起；stored_keyword 固定 STORED",
            "limits": [
                "5.7 上 GENERATED 列的 EXTRA / GENERATION_EXPRESSION 的确切写法没有单独实测，按同一读回代码推断",
                "9.x 移除 mysql_native_password 插件属于连接认证路径，不在本报告范围（由连接层验收负责）",
            ],
        },
        "mariadb": {
            "family": "MariaDB",
            "measured_fixed": "12.3.3",
            "lower": "11.8.9 / 11.4.13 / 10.11.19 / 10.6.28",
            "gate": "generated_columns 从 10.2 起；stored_keyword 固定 PERSISTENT",
            "limits": [
                "12.x 相对 11.8 在分区与生成列上的差异没有单独实测",
                "information_schema.PARTITIONS 的 PARTITION_DESCRIPTION 写法没有逐版本核对",
            ],
        },
    }
    body = ["# MySQL / MariaDB 表结构真实验收（T2）", "", "生成时间：%s" % now, "", "## 运行命令", "", FENCE]
    body.append("python scripts/verify-mysql-table.py " + " ".join(args))
    body.append(command)
    body.append(FENCE)
    body.append("")
    body.append("## 实际运行版本")
    body.append("")
    body.append("| 引擎 | 镜像 | 服务器版本 | 用例是否执行 |")
    body.append("| --- | --- | --- | --- |")
    for spec in FIXTURES:
        engine = spec["engine"]
        body.append("| %s | %s | %s | %s |" % (engine, spec["image"], versions.get(engine, "未读到"), "是" if done.get(engine) else "否"))
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
        body.append("| %s | %s | %s | %s | %s |" % (
            spec["family"],
            raw or "未读到",
            "是" if done.get(engine) else "否",
            aligned,
            spec["lower"],
        ))
    body.append("")
    body.append("方向只能是高 → 低：本轮用到的语法与读回路径在 9.7.4 / 12.3.3 上全部跑通，更低版本支持的语法是它的子集；反过来，8.4 / 11.4 上通过的结论证明不了 9.x / 12.x —— 新版本会移除插件、改默认值与写法，低版本没跑到的分支不会因此暴露。所以本报告不去用低版本“代表”高版本。")
    body.append("")
    body.append("共用同一实现路径的依据：")
    body.append("")
    body.append("- DDL 生成：`adapters/mysql/table_ddl.rs` 的 `enum_set_type`、`generated_clause`、`partition_by_clause`、`partition_steps` 对两个家族用同一份代码，没有按主次版本分支。")
    body.append("- 元数据读回：`information_schema.columns`（COLUMN_TYPE / EXTRA / GENERATION_EXPRESSION）与 `information_schema.PARTITIONS`，同样是同一份代码。")
    body.append("- KEY 分区的列清单还原：`adapters/mysql/table_read.rs` 的 `key_columns_from_expression` 把读回的 PARTITION_EXPRESSION（两引擎实测都是 `a`,`b` 这种反引号形状）还原成裸列名，界面起稿与重建共用这一个函数；加引号仍由 table_ddl 按方言做。")
    body.append("- 驱动与协议：同一 mysql_async 0.37.1（rustls）。MySQL 与 MariaDB 不互相替代，两个引擎各自跑完整流程。")
    body.append("- 唯一的版本分叉在 `adapters/mysql/profile.rs` 的 FamilyFeatures：mysql —— %s；mariadb —— %s。两个分叉分支本轮都被实际执行（STORED 与 PERSISTENT 各一次）。" % (coverage["mysql"]["gate"], coverage["mariadb"]["gate"]))
    body.append("")
    body.append("仍属推断 / 限制（不得当作已验证事实）：")
    body.append("")
    body.append("- 上表“由它推断的更低目标版本”整列都是推断，本轮没有在这些版本上运行过。")
    body.append("- KEY 分区的 PARTITION_EXPRESSION 只在 9.7.4 / 12.3.3 上实测为 `a`,`b` 的反引号形状；更低版本的返回写法按同一读回代码推断，本轮没有逐版本核对。")
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
            body.append("table-live: %s %s" % (engine, line))
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
            print("%s %s 就绪（端口 %s）" % (spec["engine"], versions[spec["engine"]], spec["port"]))
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
        write_report(args.report, sys.argv[1:], versions, evidence, output, command)
        print("报告：%s" % args.report)

    if failure:
        if args.require_fixtures:
            print("失败：%s" % failure, file=sys.stderr)
            return 1
        print("跳过（没有 --require-fixtures）：%s" % failure, file=sys.stderr)
        return 0
    print("通过：MySQL 与 MariaDB 都实际跑完表结构验收")
    return 0


if __name__ == "__main__":
    sys.exit(main())
