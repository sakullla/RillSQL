#!/bin/sh
# Redis seed（R11）：string/hash/list/set/zset/stream 各至少一键，含中文值，TTL 有差异。
# 只用 SET/HSET/RPUSH/SADD/ZADD/XADD/EXPIRE 写入；浏览端禁 KEYS，seed 本身也不使用 KEYS。
set -eu

HOST=redis
PASS=rillsql-redis
CLI="redis-cli --no-auth-warning -h $HOST -a $PASS"

# 等健康检查后的服务完全可用（healthcheck 已过，这里重试 3 次兜底）。
i=0
until [ "$($CLI ping 2>/dev/null)" = "PONG" ]; do
  i=$((i + 1))
  [ "$i" -ge 3 ] && echo "redis seed: 服务未就绪" && exit 1
  sleep 2
done

# string：一个长期、一个带 TTL（验证 PTTL 与 EXPIRE 修改后刷新可见）。
$CLI SET "rillsql:string:welcome" "澄库 Redis 夹具 · 中文值"
$CLI SET "rillsql:string:temp-token" "短时令牌"
$CLI EXPIRE "rillsql:string:temp-token" 3600

# hash：订单样例。
$CLI HSET "rillsql:hash:order:1001" status paid customer 林夏 total 128.5 note 加急

# list：任务队列。
$CLI RPUSH "rillsql:list:queue" 任务A 任务B 任务C

# set：标签集合。
$CLI SADD "rillsql:set:tags" 紧急 加急 普通

# zset：排行榜（带分值）。
$CLI ZADD "rillsql:zset:rank" 90 张伟 85 林夏 70 周宁

# stream：事件流。
$CLI XADD "rillsql:stream:events" '*' event order.created operator 管理员
$CLI XADD "rillsql:stream:events" '*' event order.shipped operator 周宁

# 另一组不同 TTL：zset 半小时过期，验证 TTL 修改（EXPIRE/PERSIST）路径。
$CLI ZADD "rillsql:zset:flash-sale" 100 林夏 95 周宁
$CLI EXPIRE "rillsql:zset:flash-sale" 1800

echo "redis seed: 完成"
$CLI DBSIZE
