-- SQL Server seed：与 postgres/mysql 夹具同风格——少量表、主键/自增/外键、一视图、一存储过程、少量数据行。
CREATE TABLE customers (
  id int IDENTITY(1, 1) PRIMARY KEY,
  name nvarchar(120) NOT NULL,
  created_at datetime2 NOT NULL DEFAULT SYSDATETIME()
);

CREATE TABLE orders (
  id int IDENTITY(1, 1) PRIMARY KEY,
  customer_id int NOT NULL REFERENCES customers (id),
  status nvarchar(20) NOT NULL,
  total decimal(10, 2) NOT NULL
);

CREATE TABLE order_lines (
  id bigint IDENTITY(1, 1) PRIMARY KEY,
  order_id int NOT NULL REFERENCES orders (id),
  item nvarchar(120) NOT NULL,
  quantity int NOT NULL
);

CREATE SEQUENCE note_seq AS int START WITH 100 INCREMENT BY 1;

CREATE VIEW paid_orders AS
SELECT id, customer_id, total
FROM orders
WHERE status = N'paid';

CREATE PROCEDURE count_paid_orders
AS
BEGIN
  SELECT COUNT(*) AS paid_count FROM paid_orders;
END;
GO

INSERT INTO customers (name) VALUES
  (N'林夏'),
  (N'周衡'),
  (N'陈木');

INSERT INTO orders (customer_id, status, total) VALUES
  (1, N'paid', 128.50),
  (1, N'pending', 42.00),
  (2, N'paid', 76.20);

INSERT INTO order_lines (order_id, item, quantity) VALUES
  (1, N'键盘', 1),
  (2, N'显示器', 2),
  (3, N'鼠标', 1);
