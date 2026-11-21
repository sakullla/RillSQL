-- MySQL / MariaDB 表结构验收夹具（T2：ENUM/SET、生成列、RANGE/LIST/HASH/KEY 分区）。
-- 由 scripts/verify-mysql-table.py 在容器启动后执行（也挂在 compose 的 initdb 上），可重复执行。
-- 只建"起点"：每种形态的增删改由验收用例自己创建，避免夹具依赖某个版本独有的写法。

CREATE DATABASE IF NOT EXISTS rillsql_proprietary CHARACTER SET utf8mb4;
USE rillsql_proprietary;

-- 普通表：验收用例在它上面建分区、加 ENUM/SET 字段与生成列。
CREATE TABLE IF NOT EXISTS orders (
    id BIGINT NOT NULL AUTO_INCREMENT,
    customer VARCHAR(64) NOT NULL,
    status VARCHAR(16) NOT NULL DEFAULT 'draft',
    amount DECIMAL(12,2) NOT NULL DEFAULT 0,
    quantity INT NOT NULL DEFAULT 1,
    created_at DATETIME NOT NULL,
    PRIMARY KEY (id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- 已有 ENUM/SET 的表：读回候选值（含空串、单引号、逗号与中文）时用它对照。
CREATE TABLE IF NOT EXISTS orders_labels (
    id BIGINT NOT NULL AUTO_INCREMENT,
    status ENUM('','it''s','a,b','中文') NOT NULL DEFAULT '',
    tags SET('x','y') NOT NULL DEFAULT 'x',
    PRIMARY KEY (id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- 已有生成列的表：VIRTUAL 与持久化两种写法各一列。
CREATE TABLE IF NOT EXISTS orders_totals (
    id BIGINT NOT NULL AUTO_INCREMENT,
    price INT NOT NULL DEFAULT 0,
    quantity INT NOT NULL DEFAULT 1,
    total INT GENERATED ALWAYS AS (price * quantity) VIRTUAL,
    PRIMARY KEY (id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
