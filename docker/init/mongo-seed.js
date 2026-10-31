// MongoDB seed（R12）：官方 /docker-entrypoint-initdb.d 机制，首次建库时以 root 执行。
// 覆盖验收：集合计数、JSON 过滤（{"status":"active"}）、分页（orders 25 篇 > 默认页大小 20）；
// 文档含中文字段、数组、嵌套对象、ObjectId 与日期，供文档查看/复制/编辑/新增/删除全链路演练。
// 不做聚合管道/GridFS/索引管理（R12 边界）。

const rillsql = db.getSiblingDB("rillsql");

rillsql.customers.drop();
rillsql.orders.drop();

rillsql.customers.insertMany([
  {
    _id: ObjectId("65f2c1e4a1b2c3d4e5f60701"),
    姓名: "林夏",
    等级: "gold",
    联系: { 城市: "杭州", 电话: "13800000001" },
    标签: ["加急", "回头客"],
    注册时间: ISODate("2026-01-05T09:30:00Z"),
  },
  {
    _id: ObjectId("65f2c1e4a1b2c3d4e5f60702"),
    姓名: "周宁",
    等级: "silver",
    联系: { 城市: "苏州", 电话: "13800000002" },
    标签: ["普通"],
    注册时间: ISODate("2026-02-14T14:05:00Z"),
  },
  {
    _id: ObjectId("65f2c1e4a1b2c3d4e5f60703"),
    姓名: "张伟",
    等级: "bronze",
    联系: { 城市: "北京", 电话: "13800000003" },
    标签: [],
    注册时间: ISODate("2026-03-20T11:45:00Z"),
  },
]);

// 25 篇订单：前 10 篇固定 active，其余（11–25）按四态取模循环（statuses[index % 4]），
// 实际分布为 active 14（前 10 + 12/16/20/24）、pending 4（13/17/21/25）、paid 3（14/18/22）、cancelled 4（11/15/19/23）。
// {"status":"active"} 过滤验收预期 14 篇；翻页（第 1 页 20 篇 + 第 2 页 5 篇）能看出顺序稳定（按 _id 升序）。
const customers = ["林夏", "周宁", "张伟"];
const statuses = ["active", "pending", "paid", "cancelled"];
const orderDocs = [];
for (let index = 1; index <= 25; index += 1) {
  const status = index <= 10 ? "active" : statuses[index % statuses.length];
  orderDocs.push({
    _id: ObjectId("65f2c1e4a1b2c3d4e5f6" + String(1000 + index)),
    单号: "SO-" + String(1000 + index),
    客户: customers[index % customers.length],
    status: status,
    总额: Number((index * 17.5).toFixed(2)),
    明细: [{ 货号: "SKU-" + index, 数量: index % 5 + 1 }],
    备注: index % 3 === 0 ? "加急" : null,
    下单时间: new Date(Date.UTC(2026, 8, (index % 27) + 1, 8, 30)),
  });
}
rillsql.orders.insertMany(orderDocs);

print("mongo seed: customers=" + rillsql.customers.countDocuments() + " orders=" + rillsql.orders.countDocuments());
