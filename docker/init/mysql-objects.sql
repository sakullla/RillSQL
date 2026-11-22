-- MySQL / MariaDB 对象验收夹具（T3：事件、函数/存储过程、触发器）。
-- 由 scripts/verify-mysql-objects.py 在容器启动后执行（也可挂到 compose 的 initdb），可重复执行。
-- 只建"起点"：每类对象的建/改/删与读回由验收用例自己完成，夹具不预先占用用例要用的名字。
-- 主体保持单条语句：这个文件由 mysql/mariadb 客户端按分号切分执行，多语句主体要写在 BEGIN … END 里由用例发送。
-- 创建函数的前提：binlog 打开时服务器要求函数声明 DETERMINISTIC / NO SQL / READS SQL DATA，或者允许
-- log_bin_trust_function_creators。对象编辑器不替用户编这些语义，所以夹具按真实部署的做法打开开关，
-- 用例里新建的函数按“没写这些声明”创建；服务器拒绝时编辑器会把这条原始错误照实显示。
SET GLOBAL log_bin_trust_function_creators = 1;

CREATE DATABASE IF NOT EXISTS rillsql_proprietary CHARACTER SET utf8mb4;
USE rillsql_proprietary;

-- 触发器与事件的作用对象。
CREATE TABLE IF NOT EXISTS orders (
    id BIGINT NOT NULL AUTO_INCREMENT,
    amount DECIMAL(12,2) NOT NULL DEFAULT 0,
    note VARCHAR(64) DEFAULT NULL,
    PRIMARY KEY (id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE IF NOT EXISTS audit_log (
    id BIGINT NOT NULL AUTO_INCREMENT,
    order_id BIGINT NOT NULL DEFAULT 0,
    action VARCHAR(32) NOT NULL DEFAULT '',
    PRIMARY KEY (id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE IF NOT EXISTS counters (
    name VARCHAR(32) NOT NULL,
    value INT NOT NULL DEFAULT 0,
    PRIMARY KEY (name)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- 已存在的事件：用例改它的调度与主体、启停、删除。SCHEDULE 用固定起点，避免依赖"现在"。
DROP EVENT IF EXISTS existing_event;
CREATE EVENT existing_event
    ON SCHEDULE EVERY 1 DAY STARTS '2026-01-01 00:00:00'
    ON COMPLETION PRESERVE
    DISABLE
    COMMENT '夹具里的既有事件'
    DO INSERT INTO audit_log (order_id, action) VALUES (0, 'existing_tick');

-- 已存在的函数与存储过程：用例读它们、改它们、删它们。
DROP FUNCTION IF EXISTS existing_function;
CREATE FUNCTION existing_function(seed INT) RETURNS INT DETERMINISTIC RETURN seed * 2;

DROP PROCEDURE IF EXISTS existing_procedure;
CREATE PROCEDURE existing_procedure(IN seed INT, OUT total INT) SET total = seed + 1;

-- 同库同名的函数与存储过程：它们是两个独立对象，读回时参数不能互相串。
DROP FUNCTION IF EXISTS same_name;
DROP PROCEDURE IF EXISTS same_name;
CREATE FUNCTION same_name(seed INT) RETURNS INT DETERMINISTIC RETURN seed * 2;
CREATE PROCEDURE same_name(IN seed INT, OUT total INT) SET total = seed + 100;

-- 已存在的触发器：用例改它的时机/主体、删除，并用一次 DML 证明绑定。
DROP TRIGGER IF EXISTS existing_trigger;
CREATE TRIGGER existing_trigger BEFORE INSERT ON orders FOR EACH ROW SET NEW.note = COALESCE(NEW.note, 'created');
