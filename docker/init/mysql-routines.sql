-- 例程定义读取夹具（R6）：定义者可读、非定义者无 SHOW_ROUTINE 时的回退与权限说明。
-- compose 的 MySQL / MariaDB 在初次建卷时执行；live 测试（adapters/mysql/routine_definition.rs）
-- 以 root 再执行一次，已存在的旧卷也能用。可重复执行，不改动 mysql.sql 的已有种子。
-- 主体保持单条语句：初始化由客户端按分号切分执行。
USE rillsql;

-- 定义者是 compose 建的应用账号 rillsql（库内全部权限，没有 SHOW_ROUTINE / 全局 SELECT）。
DROP PROCEDURE IF EXISTS rillsql_routine_proc;
CREATE DEFINER=`rillsql`@`%` PROCEDURE rillsql_routine_proc(IN seed INT, OUT total INT) READS SQL DATA COMMENT 'routine definition fixture' SET total = seed + 1;

DROP FUNCTION IF EXISTS rillsql_routine_func;
CREATE DEFINER=`rillsql`@`%` FUNCTION rillsql_routine_func(seed INT) RETURNS INT DETERMINISTIC NO SQL RETURN seed * 2;

-- 非定义者：能执行、能看到例程元数据，但服务器不返回定义（ROUTINE_DEFINITION / SHOW CREATE 为 NULL）。
CREATE USER IF NOT EXISTS 'rillsql_viewer'@'%' IDENTIFIED BY 'rillsql';
GRANT SELECT, EXECUTE ON rillsql.* TO 'rillsql_viewer'@'%';
FLUSH PRIVILEGES;
