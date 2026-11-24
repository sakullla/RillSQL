-- UX 第三轮夹具（R2/R3/R4/R13/R14）：全部为通用名称，不含任何真实环境信息。
-- compose 的 MySQL 在初次建卷时执行；可重复执行（IF NOT EXISTS / INSERT IGNORE / DROP ... IF EXISTS），
-- 旧卷可手工补跑：docker compose -f docker/compose.yaml exec -T mysql mysql -uroot -proot < docker/init/mysql-ux-round3.sql
--
-- 内容：
--   shop                 宽表 wide_orders（25 列）、联合主键表 order_lines(order_id, line_no)、
--                        存储过程 report_two_sets（两个结果集 + 一条警告）与 touch_nothing（无结果集）。
--   shop_archive         归档库，少量表，部分名称含 map。
--   shop_search_a / _b   侧栏搜索用的大量表；三个 search 相关库合计 400 张以上，名称含 map 的表分布在多个 schema。

CREATE DATABASE IF NOT EXISTS shop CHARACTER SET utf8mb4;
CREATE DATABASE IF NOT EXISTS shop_archive CHARACTER SET utf8mb4;
CREATE DATABASE IF NOT EXISTS shop_search_a CHARACTER SET utf8mb4;
CREATE DATABASE IF NOT EXISTS shop_search_b CHARACTER SET utf8mb4;

-- 应用账号 rillsql 也能浏览和编辑这些库（compose 只给了 rillsql.*）。
GRANT ALL PRIVILEGES ON shop.* TO 'rillsql'@'%';
GRANT ALL PRIVILEGES ON shop_archive.* TO 'rillsql'@'%';
GRANT ALL PRIVILEGES ON shop_search_a.* TO 'rillsql'@'%';
GRANT ALL PRIVILEGES ON shop_search_b.* TO 'rillsql'@'%';
FLUSH PRIVILEGES;

USE shop;

-- 25 列宽表（R3 列布局）。
CREATE TABLE IF NOT EXISTS wide_orders (
  id int NOT NULL PRIMARY KEY,
  col_01_int int,
  col_02_vc varchar(32),
  col_03_date date,
  col_04_amount decimal(10, 2),
  col_05_int int,
  col_06_vc varchar(32),
  col_07_date date,
  col_08_amount decimal(10, 2),
  col_09_int int,
  col_10_vc varchar(32),
  col_11_date date,
  col_12_amount decimal(10, 2),
  col_13_int int,
  col_14_vc varchar(32),
  col_15_date date,
  col_16_amount decimal(10, 2),
  col_17_int int,
  col_18_vc varchar(32),
  col_19_date date,
  col_20_amount decimal(10, 2),
  col_21_int int,
  col_22_vc varchar(32),
  col_23_date date,
  col_24_amount decimal(10, 2)
);
INSERT IGNORE INTO wide_orders (id, col_01_int, col_02_vc, col_03_date, col_04_amount, col_05_int, col_06_vc, col_07_date, col_08_amount, col_09_int, col_10_vc, col_11_date, col_12_amount, col_13_int, col_14_vc, col_15_date, col_16_amount, col_17_int, col_18_vc, col_19_date, col_20_amount, col_21_int, col_22_vc, col_23_date, col_24_amount) VALUES
  (1, 101, 'item_01_02', '2026-02-04', 14.05, 105, 'item_01_06', '2026-02-08', 18.09, 109, 'item_01_10', '2026-02-12', 22.13, 113, 'item_01_14', '2026-02-16', 26.17, 117, 'item_01_18', '2026-02-20', 30.21, 121, 'item_01_22', '2026-02-24', 34.25),
  (2, 201, 'item_02_02', '2026-03-04', 24.06, 205, 'item_02_06', '2026-03-08', 28.10, 209, 'item_02_10', '2026-03-12', 32.14, 213, 'item_02_14', '2026-03-16', 36.18, 217, 'item_02_18', '2026-03-20', 40.22, 221, 'item_02_22', '2026-03-24', 44.26),
  (3, 301, 'item_03_02', '2026-04-04', 34.07, 305, 'item_03_06', '2026-04-08', 38.11, 309, 'item_03_10', '2026-04-12', 42.15, 313, 'item_03_14', '2026-04-16', 46.19, 317, 'item_03_18', '2026-04-20', 50.23, 321, 'item_03_22', '2026-04-24', 54.27),
  (4, 401, 'item_04_02', '2026-05-04', 44.08, 405, 'item_04_06', '2026-05-08', 48.12, 409, 'item_04_10', '2026-05-12', 52.16, 413, 'item_04_14', '2026-05-16', 56.20, 417, 'item_04_18', '2026-05-20', 60.24, 421, 'item_04_22', '2026-05-24', 64.28),
  (5, 501, 'item_05_02', '2026-06-04', 54.09, 505, 'item_05_06', '2026-06-08', 58.13, 509, 'item_05_10', '2026-06-12', 62.17, 513, 'item_05_14', '2026-06-16', 66.21, 517, 'item_05_18', '2026-06-20', 70.25, 521, 'item_05_22', '2026-06-24', 74.29),
  (6, 601, 'item_06_02', '2026-07-04', 64.10, 605, 'item_06_06', '2026-07-08', 68.14, 609, 'item_06_10', '2026-07-12', 72.18, 613, 'item_06_14', '2026-07-16', 76.22, 617, 'item_06_18', '2026-07-20', 80.26, 621, 'item_06_22', '2026-07-24', 84.30),
  (7, 701, 'item_07_02', '2026-08-04', 74.11, 705, 'item_07_06', '2026-08-08', 78.15, 709, 'item_07_10', '2026-08-12', 82.19, 713, 'item_07_14', '2026-08-16', 86.23, 717, 'item_07_18', '2026-08-20', 90.27, 721, 'item_07_22', '2026-08-24', 94.31),
  (8, 801, 'item_08_02', '2026-09-04', 84.12, 805, 'item_08_06', '2026-09-08', 88.16, 809, 'item_08_10', '2026-09-12', 92.20, 813, 'item_08_14', '2026-09-16', 96.24, 817, 'item_08_18', '2026-09-20', 100.28, 821, 'item_08_22', '2026-09-24', 104.32),
  (9, 901, 'item_09_02', '2026-10-04', 94.13, 905, 'item_09_06', '2026-10-08', 98.17, 909, 'item_09_10', '2026-10-12', 102.21, 913, 'item_09_14', '2026-10-16', 106.25, 917, 'item_09_18', '2026-10-20', 110.29, 921, 'item_09_22', '2026-10-24', 114.33),
  (10, 1001, 'item_10_02', '2026-11-04', 104.14, 1005, 'item_10_06', '2026-11-08', 108.18, 1009, 'item_10_10', '2026-11-12', 112.22, 1013, 'item_10_14', '2026-11-16', 116.26, 1017, 'item_10_18', '2026-11-20', 120.30, 1021, 'item_10_22', '2026-11-24', 124.34),
  (11, 1101, 'item_11_02', '2026-12-04', 114.15, 1105, 'item_11_06', '2026-12-08', 118.19, 1109, 'item_11_10', '2026-12-12', 122.23, 1113, 'item_11_14', '2026-12-16', 126.27, 1117, 'item_11_18', '2026-12-20', 130.31, 1121, 'item_11_22', '2026-12-24', 134.35),
  (12, 1201, 'item_12_02', '2026-01-04', 124.16, 1205, 'item_12_06', '2026-01-08', 128.20, 1209, 'item_12_10', '2026-01-12', 132.24, 1213, 'item_12_14', '2026-01-16', 136.28, 1217, 'item_12_18', '2026-01-20', 140.32, 1221, 'item_12_22', '2026-01-24', 144.36),
  (13, 1301, 'item_13_02', '2026-02-04', 134.17, 1305, 'item_13_06', '2026-02-08', 138.21, 1309, 'item_13_10', '2026-02-12', 142.25, 1313, 'item_13_14', '2026-02-16', 146.29, 1317, 'item_13_18', '2026-02-20', 150.33, 1321, 'item_13_22', '2026-02-24', 154.37),
  (14, 1401, 'item_14_02', '2026-03-04', 144.18, 1405, 'item_14_06', '2026-03-08', 148.22, 1409, 'item_14_10', '2026-03-12', 152.26, 1413, 'item_14_14', '2026-03-16', 156.30, 1417, 'item_14_18', '2026-03-20', 160.34, 1421, 'item_14_22', '2026-03-24', 164.38),
  (15, 1501, 'item_15_02', '2026-04-04', 154.19, 1505, 'item_15_06', '2026-04-08', 158.23, 1509, 'item_15_10', '2026-04-12', 162.27, 1513, 'item_15_14', '2026-04-16', 166.31, 1517, 'item_15_18', '2026-04-20', 170.35, 1521, 'item_15_22', '2026-04-24', 174.39),
  (16, 1601, 'item_16_02', '2026-05-04', 164.20, 1605, 'item_16_06', '2026-05-08', 168.24, 1609, 'item_16_10', '2026-05-12', 172.28, 1613, 'item_16_14', '2026-05-16', 176.32, 1617, 'item_16_18', '2026-05-20', 180.36, 1621, 'item_16_22', '2026-05-24', 184.40),
  (17, 1701, 'item_17_02', '2026-06-04', 174.21, 1705, 'item_17_06', '2026-06-08', 178.25, 1709, 'item_17_10', '2026-06-12', 182.29, 1713, 'item_17_14', '2026-06-16', 186.33, 1717, 'item_17_18', '2026-06-20', 190.37, 1721, 'item_17_22', '2026-06-24', 194.41),
  (18, 1801, 'item_18_02', '2026-07-04', 184.22, 1805, 'item_18_06', '2026-07-08', 188.26, 1809, 'item_18_10', '2026-07-12', 192.30, 1813, 'item_18_14', '2026-07-16', 196.34, 1817, 'item_18_18', '2026-07-20', 200.38, 1821, 'item_18_22', '2026-07-24', 204.42),
  (19, 1901, 'item_19_02', '2026-08-04', 194.23, 1905, 'item_19_06', '2026-08-08', 198.27, 1909, 'item_19_10', '2026-08-12', 202.31, 1913, 'item_19_14', '2026-08-16', 206.35, 1917, 'item_19_18', '2026-08-20', 210.39, 1921, 'item_19_22', '2026-08-24', 214.43),
  (20, 2001, 'item_20_02', '2026-09-04', 204.24, 2005, 'item_20_06', '2026-09-08', 208.28, 2009, 'item_20_10', '2026-09-12', 212.32, 2013, 'item_20_14', '2026-09-16', 216.36, 2017, 'item_20_18', '2026-09-20', 220.40, 2021, 'item_20_22', '2026-09-24', 224.44),
  (21, 2101, 'item_21_02', '2026-10-04', 214.25, 2105, 'item_21_06', '2026-10-08', 218.29, 2109, 'item_21_10', '2026-10-12', 222.33, 2113, 'item_21_14', '2026-10-16', 226.37, 2117, 'item_21_18', '2026-10-20', 230.41, 2121, 'item_21_22', '2026-10-24', 234.45),
  (22, 2201, 'item_22_02', '2026-11-04', 224.26, 2205, 'item_22_06', '2026-11-08', 228.30, 2209, 'item_22_10', '2026-11-12', 232.34, 2213, 'item_22_14', '2026-11-16', 236.38, 2217, 'item_22_18', '2026-11-20', 240.42, 2221, 'item_22_22', '2026-11-24', 244.46),
  (23, 2301, 'item_23_02', '2026-12-04', 234.27, 2305, 'item_23_06', '2026-12-08', 238.31, 2309, 'item_23_10', '2026-12-12', 242.35, 2313, 'item_23_14', '2026-12-16', 246.39, 2317, 'item_23_18', '2026-12-20', 250.43, 2321, 'item_23_22', '2026-12-24', 254.47),
  (24, 2401, 'item_24_02', '2026-01-04', 244.28, 2405, 'item_24_06', '2026-01-08', 248.32, 2409, 'item_24_10', '2026-01-12', 252.36, 2413, 'item_24_14', '2026-01-16', 256.40, 2417, 'item_24_18', '2026-01-20', 260.44, 2421, 'item_24_22', '2026-01-24', 264.48),
  (25, 2501, 'item_25_02', '2026-02-04', 254.29, 2505, 'item_25_06', '2026-02-08', 258.33, 2509, 'item_25_10', '2026-02-12', 262.37, 2513, 'item_25_14', '2026-02-16', 266.41, 2517, 'item_25_18', '2026-02-20', 270.45, 2521, 'item_25_22', '2026-02-24', 274.49),
  (26, 2601, 'item_26_02', '2026-03-04', 264.30, 2605, 'item_26_06', '2026-03-08', 268.34, 2609, 'item_26_10', '2026-03-12', 272.38, 2613, 'item_26_14', '2026-03-16', 276.42, 2617, 'item_26_18', '2026-03-20', 280.46, 2621, 'item_26_22', '2026-03-24', 284.50),
  (27, 2701, 'item_27_02', '2026-04-04', 274.31, 2705, 'item_27_06', '2026-04-08', 278.35, 2709, 'item_27_10', '2026-04-12', 282.39, 2713, 'item_27_14', '2026-04-16', 286.43, 2717, 'item_27_18', '2026-04-20', 290.47, 2721, 'item_27_22', '2026-04-24', 294.51),
  (28, 2801, 'item_28_02', '2026-05-04', 284.32, 2805, 'item_28_06', '2026-05-08', 288.36, 2809, 'item_28_10', '2026-05-12', 292.40, 2813, 'item_28_14', '2026-05-16', 296.44, 2817, 'item_28_18', '2026-05-20', 300.48, 2821, 'item_28_22', '2026-05-24', 304.52),
  (29, 2901, 'item_29_02', '2026-06-04', 294.33, 2905, 'item_29_06', '2026-06-08', 298.37, 2909, 'item_29_10', '2026-06-12', 302.41, 2913, 'item_29_14', '2026-06-16', 306.45, 2917, 'item_29_18', '2026-06-20', 310.49, 2921, 'item_29_22', '2026-06-24', 314.53),
  (30, 3001, 'item_30_02', '2026-07-04', 304.34, 3005, 'item_30_06', '2026-07-08', 308.38, 3009, 'item_30_10', '2026-07-12', 312.42, 3013, 'item_30_14', '2026-07-16', 316.46, 3017, 'item_30_18', '2026-07-20', 320.50, 3021, 'item_30_22', '2026-07-24', 324.54);

-- 联合主键表（R13）。
CREATE TABLE IF NOT EXISTS order_lines (
  order_id int NOT NULL,
  line_no int NOT NULL,
  sku varchar(32) NOT NULL,
  qty int NOT NULL,
  price decimal(10, 2) NOT NULL,
  PRIMARY KEY (order_id, line_no)
);
INSERT IGNORE INTO order_lines (order_id, line_no, sku, qty, price) VALUES
  (1, 1, 'sku_11', 1, 11.50),
  (1, 2, 'sku_12', 2, 12.50),
  (1, 3, 'sku_13', 3, 13.50),
  (2, 1, 'sku_21', 1, 21.50),
  (2, 2, 'sku_22', 2, 22.50),
  (2, 3, 'sku_23', 3, 23.50),
  (3, 1, 'sku_31', 1, 31.50),
  (3, 2, 'sku_32', 2, 32.50),
  (3, 3, 'sku_33', 3, 33.50),
  (4, 1, 'sku_41', 1, 41.50),
  (4, 2, 'sku_42', 2, 42.50),
  (4, 3, 'sku_43', 3, 43.50),
  (5, 1, 'sku_51', 1, 51.50),
  (5, 2, 'sku_52', 2, 52.50),
  (5, 3, 'sku_53', 3, 53.50);

-- 存储过程（R14）：report_two_sets 依次返回两个结果集，最后以 SQLSTATE 01000 发出一条警告
-- （CALL 之后 SHOW WARNINGS 可读到）；touch_nothing 只做赋值，不返回结果集。
DROP PROCEDURE IF EXISTS report_two_sets;
DROP PROCEDURE IF EXISTS touch_nothing;
DELIMITER //
CREATE PROCEDURE report_two_sets()
BEGIN
  SELECT order_id, COUNT(*) AS line_count FROM order_lines GROUP BY order_id ORDER BY order_id;
  SELECT id, col_01_int, col_02_vc FROM wide_orders ORDER BY id LIMIT 5;
  SIGNAL SQLSTATE '01000' SET MESSAGE_TEXT = 'report_two_sets: demo warning', MYSQL_ERRNO = 1642;
END //
CREATE PROCEDURE touch_nothing()
BEGIN
  DECLARE touched int DEFAULT 0;
  SET touched = touched + 1;
END //
DELIMITER ;

-- 侧栏搜索夹具（R4）：大量表，名称含 map 的表分布在三个库中，含前缀、中缀、后缀多种位置。
USE shop_search_a;
CREATE TABLE IF NOT EXISTS sa_orders_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_customers_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_items_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_stock_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_prices_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_regions_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_stores_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS invoices_map_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_payments_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_shipments_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_returns_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_coupons_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_reviews_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_carts_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_vendors_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_channels_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_promos_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_tags_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_audits_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS map_events_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_orders_01 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_customers_01 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_items_01 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_stock_mapping_01 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_prices_01 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_regions_01 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_stores_01 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_invoices_01 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_payments_01 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_shipments_01 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_returns_01 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_coupons_01 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS reviews_map_01 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_carts_01 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_vendors_01 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_channels_01 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_promos_01 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_tags_01 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_audits_01 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_events_01 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_orders_02 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_customers_02 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_items_02 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_stock_02 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS map_prices_02 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_regions_02 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_stores_02 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_invoices_02 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_payments_mapping_02 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_shipments_02 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_returns_02 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_coupons_02 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_reviews_02 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_carts_02 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_vendors_02 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_channels_02 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_promos_02 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS tags_map_02 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_audits_02 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_events_02 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_orders_03 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_customers_03 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_items_03 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_stock_03 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_prices_03 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_regions_03 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_stores_03 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_invoices_03 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_payments_03 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS map_shipments_03 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_returns_03 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_coupons_03 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_reviews_03 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_carts_mapping_03 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_vendors_03 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_channels_03 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_promos_03 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_tags_03 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_audits_03 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_events_03 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_orders_04 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_customers_04 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS items_map_04 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_stock_04 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_prices_04 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_regions_04 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_stores_04 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_invoices_04 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_payments_04 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_shipments_04 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_returns_04 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_coupons_04 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_reviews_04 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_carts_04 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS map_vendors_04 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_channels_04 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_promos_04 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_tags_04 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_audits_mapping_04 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_events_04 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_orders_05 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_customers_05 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_items_05 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_stock_05 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_prices_05 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_regions_05 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_stores_05 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS invoices_map_05 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_payments_05 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_shipments_05 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_returns_05 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_coupons_05 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_reviews_05 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_carts_05 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_vendors_05 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_channels_05 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_promos_05 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_tags_05 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_audits_05 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS map_events_05 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_orders_06 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_customers_06 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_items_06 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_stock_mapping_06 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_prices_06 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_regions_06 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_stores_06 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_invoices_06 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_payments_06 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_shipments_06 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_returns_06 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_coupons_06 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS reviews_map_06 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_carts_06 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_vendors_06 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_channels_06 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_promos_06 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_tags_06 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_audits_06 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_events_06 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_orders_07 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_customers_07 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_items_07 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_stock_07 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS map_prices_07 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_regions_07 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_stores_07 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_invoices_07 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_payments_mapping_07 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_shipments_07 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_returns_07 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_coupons_07 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_reviews_07 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_carts_07 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_vendors_07 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_channels_07 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_promos_07 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS tags_map_07 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_audits_07 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_events_07 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_orders_08 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_customers_08 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_items_08 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_stock_08 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_prices_08 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_regions_08 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_stores_08 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_invoices_08 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_payments_08 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS map_shipments_08 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_returns_08 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_coupons_08 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_reviews_08 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_carts_mapping_08 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_vendors_08 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_channels_08 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_promos_08 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_tags_08 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_audits_08 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_events_08 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_orders_09 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_customers_09 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS items_map_09 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_stock_09 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_prices_09 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_regions_09 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_stores_09 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_invoices_09 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_payments_09 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_shipments_09 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_returns_09 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_coupons_09 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_reviews_09 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_carts_09 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS map_vendors_09 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_channels_09 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_promos_09 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_tags_09 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_audits_mapping_09 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_events_09 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_orders_10 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_customers_10 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_items_10 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_stock_10 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_prices_10 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_regions_10 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_stores_10 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS invoices_map_10 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_payments_10 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sa_shipments_10 (id int NOT NULL PRIMARY KEY, label varchar(32));

USE shop_search_b;
CREATE TABLE IF NOT EXISTS sb_orders_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_customers_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_items_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_stock_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_prices_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_regions_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_stores_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS invoices_map_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_payments_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_shipments_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_returns_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_coupons_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_reviews_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_carts_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_vendors_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_channels_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_promos_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_tags_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_audits_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS map_events_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_orders_01 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_customers_01 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_items_01 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_stock_mapping_01 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_prices_01 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_regions_01 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_stores_01 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_invoices_01 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_payments_01 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_shipments_01 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_returns_01 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_coupons_01 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS reviews_map_01 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_carts_01 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_vendors_01 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_channels_01 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_promos_01 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_tags_01 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_audits_01 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_events_01 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_orders_02 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_customers_02 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_items_02 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_stock_02 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS map_prices_02 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_regions_02 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_stores_02 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_invoices_02 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_payments_mapping_02 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_shipments_02 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_returns_02 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_coupons_02 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_reviews_02 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_carts_02 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_vendors_02 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_channels_02 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_promos_02 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS tags_map_02 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_audits_02 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_events_02 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_orders_03 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_customers_03 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_items_03 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_stock_03 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_prices_03 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_regions_03 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_stores_03 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_invoices_03 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_payments_03 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS map_shipments_03 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_returns_03 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_coupons_03 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_reviews_03 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_carts_mapping_03 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_vendors_03 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_channels_03 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_promos_03 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_tags_03 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_audits_03 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_events_03 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_orders_04 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_customers_04 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS items_map_04 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_stock_04 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_prices_04 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_regions_04 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_stores_04 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_invoices_04 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_payments_04 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_shipments_04 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_returns_04 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_coupons_04 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_reviews_04 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_carts_04 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS map_vendors_04 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_channels_04 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_promos_04 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_tags_04 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_audits_mapping_04 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_events_04 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_orders_05 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_customers_05 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_items_05 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_stock_05 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_prices_05 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_regions_05 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_stores_05 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS invoices_map_05 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_payments_05 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_shipments_05 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_returns_05 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_coupons_05 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_reviews_05 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_carts_05 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_vendors_05 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_channels_05 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_promos_05 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_tags_05 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_audits_05 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS map_events_05 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_orders_06 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_customers_06 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_items_06 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_stock_mapping_06 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_prices_06 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_regions_06 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_stores_06 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_invoices_06 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_payments_06 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_shipments_06 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_returns_06 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_coupons_06 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS reviews_map_06 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_carts_06 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_vendors_06 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_channels_06 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_promos_06 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_tags_06 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_audits_06 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_events_06 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_orders_07 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_customers_07 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_items_07 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_stock_07 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS map_prices_07 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_regions_07 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_stores_07 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_invoices_07 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_payments_mapping_07 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_shipments_07 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_returns_07 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_coupons_07 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_reviews_07 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_carts_07 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_vendors_07 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_channels_07 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_promos_07 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS tags_map_07 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_audits_07 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_events_07 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_orders_08 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_customers_08 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_items_08 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_stock_08 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_prices_08 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_regions_08 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_stores_08 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_invoices_08 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_payments_08 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS map_shipments_08 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_returns_08 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_coupons_08 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_reviews_08 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_carts_mapping_08 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_vendors_08 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_channels_08 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_promos_08 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_tags_08 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_audits_08 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_events_08 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_orders_09 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_customers_09 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS items_map_09 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_stock_09 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_prices_09 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_regions_09 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_stores_09 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_invoices_09 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_payments_09 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_shipments_09 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_returns_09 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_coupons_09 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_reviews_09 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_carts_09 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS map_vendors_09 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_channels_09 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_promos_09 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_tags_09 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_audits_mapping_09 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS sb_events_09 (id int NOT NULL PRIMARY KEY, label varchar(32));

USE shop_archive;
CREATE TABLE IF NOT EXISTS arch_orders_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS arch_customers_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS arch_items_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS arch_stock_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS arch_prices_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS arch_regions_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS arch_stores_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS invoices_map_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS arch_payments_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS arch_shipments_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS arch_returns_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS arch_coupons_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS arch_reviews_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS arch_carts_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS arch_vendors_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS arch_channels_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS arch_promos_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS arch_tags_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS arch_audits_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
CREATE TABLE IF NOT EXISTS map_events_00 (id int NOT NULL PRIMARY KEY, label varchar(32));
