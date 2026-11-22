-- MySQL / MariaDB 专有功能夹具（R1、R9）。由 docker/compose.mysql-versions.yaml 的
-- /docker-entrypoint-initdb.d/01.sql 在初始化时执行，只依赖全部目标版本都有的语法：
--   MySQL 5.7 / 8.0 / 8.4 / 9.x，MariaDB 10.6 / 10.11 / 11.4 / 11.8 / 12.x
-- 这里只建"起点"对象：ENUM/SET、生成列、分区与事件/例程/触发器的真实增删改由各自的验收脚本创建，
-- 避免夹具本身依赖某个版本独有的写法。

CREATE DATABASE IF NOT EXISTS rillsql_proprietary;
USE rillsql_proprietary;

-- 普通表：ENUM/SET、生成列与分区都以它为起点。
CREATE TABLE IF NOT EXISTS orders (
    id BIGINT NOT NULL AUTO_INCREMENT,
    customer VARCHAR(64) NOT NULL,
    status VARCHAR(16) NOT NULL DEFAULT 'draft',
    region VARCHAR(16) NOT NULL DEFAULT 'east',
    amount DECIMAL(12,2) NOT NULL DEFAULT 0,
    quantity INT NOT NULL DEFAULT 1,
    created_at DATETIME NOT NULL,
    PRIMARY KEY (id),
    KEY idx_orders_created (created_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- 事件与触发器的目标表。触发器只做数据留痕，便于验收时用一次 DML 证明绑定关系。
CREATE TABLE IF NOT EXISTS orders_audit (
    id BIGINT NOT NULL AUTO_INCREMENT,
    order_id BIGINT NOT NULL,
    note VARCHAR(255) NOT NULL,
    PRIMARY KEY (id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- 示例数据：让"删分区丢数据""生成列重算"这类风险有真实行可以观察。
INSERT INTO orders (customer, status, region, amount, quantity, created_at) VALUES
    ('alpha', 'draft', 'east', 10.00, 2, '2026-01-05 09:00:00'),
    ('beta', 'paid', 'west', 20.50, 1, '2026-02-11 10:30:00'),
    ('gamma', 'paid', 'east', 30.00, 5, '2026-03-20 11:45:00');

-- 事件调度器默认关闭：验收要观察"启用事件也不会运行"的行为，不在夹具里打开它。
