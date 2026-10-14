USE rillsql;

CREATE TABLE customers (
  id int PRIMARY KEY,
  name varchar(64) NOT NULL
);

CREATE TABLE orders (
  id int PRIMARY KEY,
  customer_id int,
  status varchar(32) NOT NULL,
  total decimal(10, 2) NOT NULL,
  CONSTRAINT orders_customer_fk FOREIGN KEY (customer_id) REFERENCES customers (id)
);

INSERT INTO customers (id, name) VALUES
  (1, '林夏'),
  (2, '周衡');

INSERT INTO orders (id, customer_id, status, total) VALUES
  (1, 1, 'paid', 128.50),
  (2, 1, 'pending', 42.00),
  (3, 2, 'paid', 76.20);
