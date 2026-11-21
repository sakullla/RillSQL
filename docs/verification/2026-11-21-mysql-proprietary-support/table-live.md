# MySQL / MariaDB 表结构真实验收（T2）

生成时间：2026-11-21 15:59:14 +0800

## 运行命令

```
python scripts/verify-mysql-table.py --require-fixtures --report docs/verification/2026-11-21-mysql-proprietary-support/table-live.md
cargo test --manifest-path src-tauri\Cargo.toml --locked --lib -- --ignored --nocapture table_features_round_trip_on_real_servers
```

## 实际运行版本

| 引擎 | 镜像 | 服务器版本 | 用例是否执行 |
| --- | --- | --- | --- |
| mysql | mysql:9.7.4 | 9.7.4 | 是 |
| mariadb | mariadb:12.3.3 | 12.3.3-MariaDB-ubu2404 | 是 |

## 功能、实现路径与覆盖关系

| 功能 | 代码路径 | 元数据 / 驱动路径 | 本轮实际覆盖的行为 |
| --- | --- | --- | --- |
| ENUM/SET 候选值 | adapters/mysql/table_ddl.rs enum_set_type、literal.rs | information_schema.columns.COLUMN_TYPE | 建 ENUM/SET 含空串、单引号、反斜杠、逗号与中文；改候选值；删字段；空候选值在预览前阻断 |
| 生成列（VIRTUAL；STORED/PERSISTENT） | adapters/mysql/table_ddl.rs generated_clause、profile.stored_keyword | information_schema.columns.EXTRA / GENERATION_EXPRESSION | 建 VIRTUAL 生成列；追加持久化生成列；改表达式；表达式默认值不被当成生成列 |
| RANGE 分区 | adapters/mysql/table_ddl.rs partition_by_clause、partition_steps | information_schema.PARTITIONS | 建立、新增、删除、重组 |
| LIST 分区 | 同上 | information_schema.PARTITIONS | 建立 |
| HASH 分区 | 同上 | information_schema.PARTITIONS | 建立、按数量增、按数量减 |
| KEY 分区 | 同上 | information_schema.PARTITIONS（PARTITION_EXPRESSION 是反引号包住的列清单） | 建立、读回列清单还原成裸列名后按同一函数重建、移除分区方案 |
| 失败路径与风险 | ddl/exec.rs 逐条执行与 receipts；table_ddl.rs partition_applicability | 服务器原始错误 | 服务器拒绝新增分区后结构不变、原始错误保留；未分区的表删除分区在预览前阻断 |

驱动：mysql_async 0.37.1（rustls）。MySQL 与 MariaDB 不互相替代：两个引擎各自跑完整流程。

## 覆盖关系（R9：最高版本实测 → 低版本按同一实现路径 + 版本门控推断）

每组只在实施时的**最高目标版本**上实测，更低版本不单独起容器：由高版本的结果加上同一实现路径与版本门控推断。推断项一律标为推断，不当成已验证。

| 家族 | 实测版本（服务器 VERSION()） | 本轮是否执行 | 与基线固定版本一致 | 由它推断的更低目标版本 |
| --- | --- | --- | --- | --- |
| MySQL | 9.7.4 | 是 | 是（9.7.4） | 8.4.13 / 8.0.46 / 5.7.44 |
| MariaDB | 12.3.3-MariaDB-ubu2404 | 是 | 是（12.3.3） | 11.8.9 / 11.4.13 / 10.11.19 / 10.6.28 |

方向只能是高 → 低：本轮用到的语法与读回路径在 9.7.4 / 12.3.3 上全部跑通，更低版本支持的语法是它的子集；反过来，8.4 / 11.4 上通过的结论证明不了 9.x / 12.x —— 新版本会移除插件、改默认值与写法，低版本没跑到的分支不会因此暴露。所以本报告不去用低版本“代表”高版本。

共用同一实现路径的依据：

- DDL 生成：`adapters/mysql/table_ddl.rs` 的 `enum_set_type`、`generated_clause`、`partition_by_clause`、`partition_steps` 对两个家族用同一份代码，没有按主次版本分支。
- 元数据读回：`information_schema.columns`（COLUMN_TYPE / EXTRA / GENERATION_EXPRESSION）与 `information_schema.PARTITIONS`，同样是同一份代码。
- KEY 分区的列清单还原：`adapters/mysql/table_read.rs` 的 `key_columns_from_expression` 把读回的 PARTITION_EXPRESSION（两引擎实测都是 `a`,`b` 这种反引号形状）还原成裸列名，界面起稿与重建共用这一个函数；加引号仍由 table_ddl 按方言做。
- 驱动与协议：同一 mysql_async 0.37.1（rustls）。MySQL 与 MariaDB 不互相替代，两个引擎各自跑完整流程。
- 唯一的版本分叉在 `adapters/mysql/profile.rs` 的 FamilyFeatures：mysql —— generated_columns 从 5.7 起；stored_keyword 固定 STORED；mariadb —— generated_columns 从 10.2 起；stored_keyword 固定 PERSISTENT。两个分叉分支本轮都被实际执行（STORED 与 PERSISTENT 各一次）。

仍属推断 / 限制（不得当作已验证事实）：

- 上表“由它推断的更低目标版本”整列都是推断，本轮没有在这些版本上运行过。
- KEY 分区的 PARTITION_EXPRESSION 只在 9.7.4 / 12.3.3 上实测为 `a`,`b` 的反引号形状；更低版本的返回写法按同一读回代码推断，本轮没有逐版本核对。
- MySQL：5.7 上 GENERATED 列的 EXTRA / GENERATION_EXPRESSION 的确切写法没有单独实测，按同一读回代码推断。
- MySQL：9.x 移除 mysql_native_password 插件属于连接认证路径，不在本报告范围（由连接层验收负责）。
- MariaDB：12.x 相对 11.8 在分区与生成列上的差异没有单独实测。
- MariaDB：information_schema.PARTITIONS 的 PARTITION_DESCRIPTION 写法没有逐版本核对。
- 本报告只证明“实测版本”一列。Delivery 记录覆盖范围时必须照抄这里的分界，不能写成“已验证全部目标版本”。

## 用例证据（用例自身打印）

```
table-live: mysql 建表 t2_enum_mysql_45944 -> CREATE TABLE rillsql_proprietary.t2_enum_mysql_45944 (   id bigint NOT NULL AUTO_INCREMENT,   status enum('','it''s','a\\b','a,b','中文'),   tags set('x','y''.z'),   PRIMARY KEY (id) ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
table-live: mysql ENUM 候选值读回 ["", "it's", "a\\b", "a,b", "中文"]
table-live: mysql 修改 ENUM -> ALTER TABLE rillsql_proprietary.t2_enum_mysql_45944 CHANGE COLUMN status status enum('','it''s','a\\b','a,b','中文','archived');
table-live: mysql 空候选值被阻断：["字段 status 是 ENUM，至少要有一个候选值。"]
table-live: mysql 建表 t2_gen_mysql_45944 -> CREATE TABLE rillsql_proprietary.t2_gen_mysql_45944 (   id bigint NOT NULL AUTO_INCREMENT,   price int NOT NULL DEFAULT 0,   quantity int NOT NULL DEFAULT 1,   total int GENERATED ALWAYS AS (price * quantity) VIRTUAL,   created_at datetime DEFAULT CURRENT_TIMESTAMP,   PRIMARY KEY (id) ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
table-live: mysql VIRTUAL 生成列读回 (`price` * `quantity`) (VIRTUAL)
table-live: mysql 新增持久化生成列 -> ALTER TABLE rillsql_proprietary.t2_gen_mysql_45944 ADD COLUMN total_x2 int GENERATED ALWAYS AS (total * 2) STORED;
table-live: mysql 持久化生成列读回 (STORED)
table-live: mysql 删除持久化生成列 -> ALTER TABLE rillsql_proprietary.t2_gen_mysql_45944 DROP COLUMN total_x2;
table-live: mysql 删除 VIRTUAL 生成列 -> ALTER TABLE rillsql_proprietary.t2_gen_mysql_45944 DROP COLUMN total;
table-live: mysql 生成列删除后读回不含 total，剩余列 ["id", "price", "quantity", "created_at"]
table-live: mysql 建表 t2_part_mysql_45944 -> CREATE TABLE rillsql_proprietary.t2_part_mysql_45944 (   id bigint NOT NULL AUTO_INCREMENT,   PRIMARY KEY (id) ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
table-live: mysql RANGE 建立 -> ALTER TABLE rillsql_proprietary.t2_part_mysql_45944 PARTITION BY RANGE(id) (PARTITION p0 VALUES LESS THAN (100), PARTITION p1 VALUES LESS THAN (200));
table-live: mysql RANGE 新增 -> ALTER TABLE rillsql_proprietary.t2_part_mysql_45944 ADD PARTITION (PARTITION p2 VALUES LESS THAN (MAXVALUE));
table-live: mysql RANGE 删除 -> ALTER TABLE rillsql_proprietary.t2_part_mysql_45944 DROP PARTITION p2;
table-live: mysql RANGE 重组 -> ALTER TABLE rillsql_proprietary.t2_part_mysql_45944 REORGANIZE PARTITION p1 INTO (PARTITION p1a VALUES LESS THAN (150), PARTITION p1b VALUES LESS THAN (MAXVALUE));
table-live: mysql LIST 建立 -> ALTER TABLE rillsql_proprietary.t2_part_mysql_45944 PARTITION BY LIST(id) (PARTITION p_low VALUES IN (1,2,3), PARTITION p_rest VALUES IN (4,5,6));
table-live: mysql HASH 建立 -> ALTER TABLE rillsql_proprietary.t2_part_mysql_45944 PARTITION BY HASH(id) PARTITIONS 4;
table-live: mysql HASH 增加数量 -> ALTER TABLE rillsql_proprietary.t2_part_mysql_45944 ADD PARTITION PARTITIONS 2;
table-live: mysql HASH 减少数量 -> ALTER TABLE rillsql_proprietary.t2_part_mysql_45944 COALESCE PARTITION 3;
table-live: mysql KEY 建立 -> ALTER TABLE rillsql_proprietary.t2_part_mysql_45944 PARTITION BY KEY(id) PARTITIONS 3;
table-live: mysql 移除分区 -> ALTER TABLE rillsql_proprietary.t2_part_mysql_45944 REMOVE PARTITIONING;
table-live: mysql 建表 t2_part_key_mysql_45944 -> CREATE TABLE rillsql_proprietary.t2_part_key_mysql_45944 (   a int NOT NULL,   b int NOT NULL,   PRIMARY KEY (a, b) ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
table-live: mysql KEY 两列建立 -> ALTER TABLE rillsql_proprietary.t2_part_key_mysql_45944 PARTITION BY KEY(a, b) PARTITIONS 3;
table-live: mysql KEY 读回列清单 "`a`,`b`" -> 裸列名 ["a", "b"]
table-live: mysql KEY 按读回重建 -> ALTER TABLE rillsql_proprietary.t2_part_key_mysql_45944 PARTITION BY KEY(a, b) PARTITIONS 3;
table-live: mysql KEY 重建后读回 3 个分区
table-live: mysql 建表 t2_part_fail_mysql_45944 -> CREATE TABLE rillsql_proprietary.t2_part_fail_mysql_45944 (   id bigint NOT NULL AUTO_INCREMENT,   PRIMARY KEY (id) ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
table-live: mysql 服务器拒绝新增分区：ERROR 1481 (HY000): MAXVALUE can only be used in last partition definition
table-live: mysql 建表 t2_part_plain_mysql_45944 -> CREATE TABLE rillsql_proprietary.t2_part_plain_mysql_45944 (   id bigint NOT NULL AUTO_INCREMENT,   PRIMARY KEY (id) ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
table-live: mysql 未分区的表被阻断：["表 t2_part_plain_mysql_45944 还没有分区方案，不能删除分区；先用“建立分区方案”分区。"]
table-live: mysql 全部用例完成
table-live: mariadb 建表 t2_enum_mariadb_45944 -> CREATE TABLE rillsql_proprietary.t2_enum_mariadb_45944 (   id bigint NOT NULL AUTO_INCREMENT,   status enum('','it''s','a\\b','a,b','中文'),   tags set('x','y''.z'),   PRIMARY KEY (id) ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
table-live: mariadb ENUM 候选值读回 ["", "it's", "a\\b", "a,b", "中文"]
table-live: mariadb 修改 ENUM -> ALTER TABLE rillsql_proprietary.t2_enum_mariadb_45944 CHANGE COLUMN status status enum('','it''s','a\\b','a,b','中文','archived') DEFAULT NULL;
table-live: mariadb 空候选值被阻断：["字段 status 是 ENUM，至少要有一个候选值。"]
table-live: mariadb 建表 t2_gen_mariadb_45944 -> CREATE TABLE rillsql_proprietary.t2_gen_mariadb_45944 (   id bigint NOT NULL AUTO_INCREMENT,   price int NOT NULL DEFAULT 0,   quantity int NOT NULL DEFAULT 1,   total int GENERATED ALWAYS AS (price * quantity) VIRTUAL,   created_at datetime DEFAULT CURRENT_TIMESTAMP,   PRIMARY KEY (id) ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
table-live: mariadb VIRTUAL 生成列读回 `price` * `quantity` (VIRTUAL)
table-live: mariadb 新增持久化生成列 -> ALTER TABLE rillsql_proprietary.t2_gen_mariadb_45944 ADD COLUMN total_x2 int GENERATED ALWAYS AS (total * 2) PERSISTENT;
table-live: mariadb 持久化生成列读回 (STORED)
table-live: mariadb 删除持久化生成列 -> ALTER TABLE rillsql_proprietary.t2_gen_mariadb_45944 DROP COLUMN total_x2;
table-live: mariadb 删除 VIRTUAL 生成列 -> ALTER TABLE rillsql_proprietary.t2_gen_mariadb_45944 DROP COLUMN total;
table-live: mariadb 生成列删除后读回不含 total，剩余列 ["id", "price", "quantity", "created_at"]
table-live: mariadb 建表 t2_part_mariadb_45944 -> CREATE TABLE rillsql_proprietary.t2_part_mariadb_45944 (   id bigint NOT NULL AUTO_INCREMENT,   PRIMARY KEY (id) ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
table-live: mariadb RANGE 建立 -> ALTER TABLE rillsql_proprietary.t2_part_mariadb_45944 PARTITION BY RANGE(id) (PARTITION p0 VALUES LESS THAN (100), PARTITION p1 VALUES LESS THAN (200));
table-live: mariadb RANGE 新增 -> ALTER TABLE rillsql_proprietary.t2_part_mariadb_45944 ADD PARTITION (PARTITION p2 VALUES LESS THAN (MAXVALUE));
table-live: mariadb RANGE 删除 -> ALTER TABLE rillsql_proprietary.t2_part_mariadb_45944 DROP PARTITION p2;
table-live: mariadb RANGE 重组 -> ALTER TABLE rillsql_proprietary.t2_part_mariadb_45944 REORGANIZE PARTITION p1 INTO (PARTITION p1a VALUES LESS THAN (150), PARTITION p1b VALUES LESS THAN (MAXVALUE));
table-live: mariadb LIST 建立 -> ALTER TABLE rillsql_proprietary.t2_part_mariadb_45944 PARTITION BY LIST(id) (PARTITION p_low VALUES IN (1,2,3), PARTITION p_rest VALUES IN (4,5,6));
table-live: mariadb HASH 建立 -> ALTER TABLE rillsql_proprietary.t2_part_mariadb_45944 PARTITION BY HASH(id) PARTITIONS 4;
table-live: mariadb HASH 增加数量 -> ALTER TABLE rillsql_proprietary.t2_part_mariadb_45944 ADD PARTITION PARTITIONS 2;
table-live: mariadb HASH 减少数量 -> ALTER TABLE rillsql_proprietary.t2_part_mariadb_45944 COALESCE PARTITION 3;
table-live: mariadb KEY 建立 -> ALTER TABLE rillsql_proprietary.t2_part_mariadb_45944 PARTITION BY KEY(id) PARTITIONS 3;
table-live: mariadb 移除分区 -> ALTER TABLE rillsql_proprietary.t2_part_mariadb_45944 REMOVE PARTITIONING;
table-live: mariadb 建表 t2_part_key_mariadb_45944 -> CREATE TABLE rillsql_proprietary.t2_part_key_mariadb_45944 (   a int NOT NULL,   b int NOT NULL,   PRIMARY KEY (a, b) ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
table-live: mariadb KEY 两列建立 -> ALTER TABLE rillsql_proprietary.t2_part_key_mariadb_45944 PARTITION BY KEY(a, b) PARTITIONS 3;
table-live: mariadb KEY 读回列清单 "`a`,`b`" -> 裸列名 ["a", "b"]
table-live: mariadb KEY 按读回重建 -> ALTER TABLE rillsql_proprietary.t2_part_key_mariadb_45944 PARTITION BY KEY(a, b) PARTITIONS 3;
table-live: mariadb KEY 重建后读回 3 个分区
table-live: mariadb 建表 t2_part_fail_mariadb_45944 -> CREATE TABLE rillsql_proprietary.t2_part_fail_mariadb_45944 (   id bigint NOT NULL AUTO_INCREMENT,   PRIMARY KEY (id) ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
table-live: mariadb 服务器拒绝新增分区：ERROR 1481 (HY000): MAXVALUE can only be used in last partition definition
table-live: mariadb 建表 t2_part_plain_mariadb_45944 -> CREATE TABLE rillsql_proprietary.t2_part_plain_mariadb_45944 (   id bigint NOT NULL AUTO_INCREMENT,   PRIMARY KEY (id) ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
table-live: mariadb 未分区的表被阻断：["表 t2_part_plain_mariadb_45944 还没有分区方案，不能删除分区；先用“建立分区方案”分区。"]
table-live: mariadb 全部用例完成
```

## 测试结果与原始输出（末尾 80 行）

```
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1625 filtered out; finished in 9.35s
running 1 test
table-live: mysql 建表 t2_enum_mysql_45944 -> CREATE TABLE rillsql_proprietary.t2_enum_mysql_45944 (   id bigint NOT NULL AUTO_INCREMENT,   status enum('','it''s','a\\b','a,b','中文'),   tags set('x','y''.z'),   PRIMARY KEY (id) ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
table-live: mysql ENUM 候选值读回 ["", "it's", "a\\b", "a,b", "中文"]
table-live: mysql 修改 ENUM -> ALTER TABLE rillsql_proprietary.t2_enum_mysql_45944 CHANGE COLUMN status status enum('','it''s','a\\b','a,b','中文','archived');
table-live: mysql 空候选值被阻断：["字段 status 是 ENUM，至少要有一个候选值。"]
table-live: mysql 建表 t2_gen_mysql_45944 -> CREATE TABLE rillsql_proprietary.t2_gen_mysql_45944 (   id bigint NOT NULL AUTO_INCREMENT,   price int NOT NULL DEFAULT 0,   quantity int NOT NULL DEFAULT 1,   total int GENERATED ALWAYS AS (price * quantity) VIRTUAL,   created_at datetime DEFAULT CURRENT_TIMESTAMP,   PRIMARY KEY (id) ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
table-live: mysql VIRTUAL 生成列读回 (`price` * `quantity`) (VIRTUAL)
table-live: mysql 新增持久化生成列 -> ALTER TABLE rillsql_proprietary.t2_gen_mysql_45944 ADD COLUMN total_x2 int GENERATED ALWAYS AS (total * 2) STORED;
table-live: mysql 持久化生成列读回 (STORED)
table-live: mysql 删除持久化生成列 -> ALTER TABLE rillsql_proprietary.t2_gen_mysql_45944 DROP COLUMN total_x2;
table-live: mysql 删除 VIRTUAL 生成列 -> ALTER TABLE rillsql_proprietary.t2_gen_mysql_45944 DROP COLUMN total;
table-live: mysql 生成列删除后读回不含 total，剩余列 ["id", "price", "quantity", "created_at"]
table-live: mysql 建表 t2_part_mysql_45944 -> CREATE TABLE rillsql_proprietary.t2_part_mysql_45944 (   id bigint NOT NULL AUTO_INCREMENT,   PRIMARY KEY (id) ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
table-live: mysql RANGE 建立 -> ALTER TABLE rillsql_proprietary.t2_part_mysql_45944 PARTITION BY RANGE(id) (PARTITION p0 VALUES LESS THAN (100), PARTITION p1 VALUES LESS THAN (200));
table-live: mysql RANGE 新增 -> ALTER TABLE rillsql_proprietary.t2_part_mysql_45944 ADD PARTITION (PARTITION p2 VALUES LESS THAN (MAXVALUE));
table-live: mysql RANGE 删除 -> ALTER TABLE rillsql_proprietary.t2_part_mysql_45944 DROP PARTITION p2;
table-live: mysql RANGE 重组 -> ALTER TABLE rillsql_proprietary.t2_part_mysql_45944 REORGANIZE PARTITION p1 INTO (PARTITION p1a VALUES LESS THAN (150), PARTITION p1b VALUES LESS THAN (MAXVALUE));
table-live: mysql LIST 建立 -> ALTER TABLE rillsql_proprietary.t2_part_mysql_45944 PARTITION BY LIST(id) (PARTITION p_low VALUES IN (1,2,3), PARTITION p_rest VALUES IN (4,5,6));
table-live: mysql HASH 建立 -> ALTER TABLE rillsql_proprietary.t2_part_mysql_45944 PARTITION BY HASH(id) PARTITIONS 4;
table-live: mysql HASH 增加数量 -> ALTER TABLE rillsql_proprietary.t2_part_mysql_45944 ADD PARTITION PARTITIONS 2;
table-live: mysql HASH 减少数量 -> ALTER TABLE rillsql_proprietary.t2_part_mysql_45944 COALESCE PARTITION 3;
table-live: mysql KEY 建立 -> ALTER TABLE rillsql_proprietary.t2_part_mysql_45944 PARTITION BY KEY(id) PARTITIONS 3;
table-live: mysql 移除分区 -> ALTER TABLE rillsql_proprietary.t2_part_mysql_45944 REMOVE PARTITIONING;
table-live: mysql 建表 t2_part_key_mysql_45944 -> CREATE TABLE rillsql_proprietary.t2_part_key_mysql_45944 (   a int NOT NULL,   b int NOT NULL,   PRIMARY KEY (a, b) ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
table-live: mysql KEY 两列建立 -> ALTER TABLE rillsql_proprietary.t2_part_key_mysql_45944 PARTITION BY KEY(a, b) PARTITIONS 3;
table-live: mysql KEY 读回列清单 "`a`,`b`" -> 裸列名 ["a", "b"]
table-live: mysql KEY 按读回重建 -> ALTER TABLE rillsql_proprietary.t2_part_key_mysql_45944 PARTITION BY KEY(a, b) PARTITIONS 3;
table-live: mysql KEY 重建后读回 3 个分区
table-live: mysql 建表 t2_part_fail_mysql_45944 -> CREATE TABLE rillsql_proprietary.t2_part_fail_mysql_45944 (   id bigint NOT NULL AUTO_INCREMENT,   PRIMARY KEY (id) ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
table-live: mysql 服务器拒绝新增分区：ERROR 1481 (HY000): MAXVALUE can only be used in last partition definition
table-live: mysql 建表 t2_part_plain_mysql_45944 -> CREATE TABLE rillsql_proprietary.t2_part_plain_mysql_45944 (   id bigint NOT NULL AUTO_INCREMENT,   PRIMARY KEY (id) ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
table-live: mysql 未分区的表被阻断：["表 t2_part_plain_mysql_45944 还没有分区方案，不能删除分区；先用“建立分区方案”分区。"]
table-live: mysql 全部用例完成
table-live: mariadb 建表 t2_enum_mariadb_45944 -> CREATE TABLE rillsql_proprietary.t2_enum_mariadb_45944 (   id bigint NOT NULL AUTO_INCREMENT,   status enum('','it''s','a\\b','a,b','中文'),   tags set('x','y''.z'),   PRIMARY KEY (id) ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
table-live: mariadb ENUM 候选值读回 ["", "it's", "a\\b", "a,b", "中文"]
table-live: mariadb 修改 ENUM -> ALTER TABLE rillsql_proprietary.t2_enum_mariadb_45944 CHANGE COLUMN status status enum('','it''s','a\\b','a,b','中文','archived') DEFAULT NULL;
table-live: mariadb 空候选值被阻断：["字段 status 是 ENUM，至少要有一个候选值。"]
table-live: mariadb 建表 t2_gen_mariadb_45944 -> CREATE TABLE rillsql_proprietary.t2_gen_mariadb_45944 (   id bigint NOT NULL AUTO_INCREMENT,   price int NOT NULL DEFAULT 0,   quantity int NOT NULL DEFAULT 1,   total int GENERATED ALWAYS AS (price * quantity) VIRTUAL,   created_at datetime DEFAULT CURRENT_TIMESTAMP,   PRIMARY KEY (id) ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
table-live: mariadb VIRTUAL 生成列读回 `price` * `quantity` (VIRTUAL)
table-live: mariadb 新增持久化生成列 -> ALTER TABLE rillsql_proprietary.t2_gen_mariadb_45944 ADD COLUMN total_x2 int GENERATED ALWAYS AS (total * 2) PERSISTENT;
table-live: mariadb 持久化生成列读回 (STORED)
table-live: mariadb 删除持久化生成列 -> ALTER TABLE rillsql_proprietary.t2_gen_mariadb_45944 DROP COLUMN total_x2;
table-live: mariadb 删除 VIRTUAL 生成列 -> ALTER TABLE rillsql_proprietary.t2_gen_mariadb_45944 DROP COLUMN total;
table-live: mariadb 生成列删除后读回不含 total，剩余列 ["id", "price", "quantity", "created_at"]
table-live: mariadb 建表 t2_part_mariadb_45944 -> CREATE TABLE rillsql_proprietary.t2_part_mariadb_45944 (   id bigint NOT NULL AUTO_INCREMENT,   PRIMARY KEY (id) ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
table-live: mariadb RANGE 建立 -> ALTER TABLE rillsql_proprietary.t2_part_mariadb_45944 PARTITION BY RANGE(id) (PARTITION p0 VALUES LESS THAN (100), PARTITION p1 VALUES LESS THAN (200));
table-live: mariadb RANGE 新增 -> ALTER TABLE rillsql_proprietary.t2_part_mariadb_45944 ADD PARTITION (PARTITION p2 VALUES LESS THAN (MAXVALUE));
table-live: mariadb RANGE 删除 -> ALTER TABLE rillsql_proprietary.t2_part_mariadb_45944 DROP PARTITION p2;
table-live: mariadb RANGE 重组 -> ALTER TABLE rillsql_proprietary.t2_part_mariadb_45944 REORGANIZE PARTITION p1 INTO (PARTITION p1a VALUES LESS THAN (150), PARTITION p1b VALUES LESS THAN (MAXVALUE));
table-live: mariadb LIST 建立 -> ALTER TABLE rillsql_proprietary.t2_part_mariadb_45944 PARTITION BY LIST(id) (PARTITION p_low VALUES IN (1,2,3), PARTITION p_rest VALUES IN (4,5,6));
table-live: mariadb HASH 建立 -> ALTER TABLE rillsql_proprietary.t2_part_mariadb_45944 PARTITION BY HASH(id) PARTITIONS 4;
table-live: mariadb HASH 增加数量 -> ALTER TABLE rillsql_proprietary.t2_part_mariadb_45944 ADD PARTITION PARTITIONS 2;
table-live: mariadb HASH 减少数量 -> ALTER TABLE rillsql_proprietary.t2_part_mariadb_45944 COALESCE PARTITION 3;
table-live: mariadb KEY 建立 -> ALTER TABLE rillsql_proprietary.t2_part_mariadb_45944 PARTITION BY KEY(id) PARTITIONS 3;
table-live: mariadb 移除分区 -> ALTER TABLE rillsql_proprietary.t2_part_mariadb_45944 REMOVE PARTITIONING;
table-live: mariadb 建表 t2_part_key_mariadb_45944 -> CREATE TABLE rillsql_proprietary.t2_part_key_mariadb_45944 (   a int NOT NULL,   b int NOT NULL,   PRIMARY KEY (a, b) ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
table-live: mariadb KEY 两列建立 -> ALTER TABLE rillsql_proprietary.t2_part_key_mariadb_45944 PARTITION BY KEY(a, b) PARTITIONS 3;
table-live: mariadb KEY 读回列清单 "`a`,`b`" -> 裸列名 ["a", "b"]
table-live: mariadb KEY 按读回重建 -> ALTER TABLE rillsql_proprietary.t2_part_key_mariadb_45944 PARTITION BY KEY(a, b) PARTITIONS 3;
table-live: mariadb KEY 重建后读回 3 个分区
table-live: mariadb 建表 t2_part_fail_mariadb_45944 -> CREATE TABLE rillsql_proprietary.t2_part_fail_mariadb_45944 (   id bigint NOT NULL AUTO_INCREMENT,   PRIMARY KEY (id) ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
table-live: mariadb 服务器拒绝新增分区：ERROR 1481 (HY000): MAXVALUE can only be used in last partition definition
table-live: mariadb 建表 t2_part_plain_mariadb_45944 -> CREATE TABLE rillsql_proprietary.t2_part_plain_mariadb_45944 (   id bigint NOT NULL AUTO_INCREMENT,   PRIMARY KEY (id) ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
table-live: mariadb 未分区的表被阻断：["表 t2_part_plain_mariadb_45944 还没有分区方案，不能删除分区；先用“建立分区方案”分区。"]
table-live: mariadb 全部用例完成
test adapters::mysql::table_live_tests::table_features_round_trip_on_real_servers ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1625 filtered out; finished in 9.35s

    Blocking waiting for file lock on build directory
   Compiling rillsql v0.3.1 (C:\Users\12976\project\RillSQL\src-tauri)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 5m 45s
     Running unittests src\lib.rs (C:/Users/12976/.cargo/target\debug\deps\rillsql_lib-b488e4f6c69edc93.exe)
```
