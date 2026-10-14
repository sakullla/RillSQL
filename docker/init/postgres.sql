CREATE TABLE customers (
  id integer PRIMARY KEY,
  name text NOT NULL
);

CREATE TABLE orders (
  id integer PRIMARY KEY,
  customer_id integer REFERENCES customers (id),
  status text NOT NULL,
  total numeric NOT NULL
);

INSERT INTO customers (id, name) VALUES
  (1, '林夏'),
  (2, '周衡');

INSERT INTO orders (id, customer_id, status, total) VALUES
  (1, 1, 'paid', 128.50),
  (2, 1, 'pending', 42.00),
  (3, 2, 'paid', 76.20);
