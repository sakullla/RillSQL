//! 真实 PostgreSQL 上的索引读取（R11）：键列升降序、INCLUDE 列不混进键列、访问方法与会丢的属性。
//! 默认被 #[ignore] 跳过，需要 `docker compose -f docker/compose.yaml up -d postgres`（127.0.0.1:5432）。

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "需要 docker/compose.yaml 的 PostgreSQL（5432）"]
async fn postgres_indexes_read_order_method_and_what_a_rebuild_would_lose() {
    let mut config = tokio_postgres::Config::new();
    config.host("127.0.0.1").port(5432).user("postgres").password("postgres").dbname("rillsql");
    let (client, connection) = config.connect(tokio_postgres::NoTls).await.expect("连接 PostgreSQL");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let table = format!("r11_idx_{}", uuid::Uuid::new_v4().simple());
    client
        .batch_execute(&format!(
            r#"CREATE TABLE public.{table} (id int PRIMARY KEY, tags text[], a text, "B c" text, d int, e int);
            CREATE INDEX {table}_g ON public.{table} USING gin (tags);
            CREATE INDEX {table}_plain ON public.{table} (a, "B c" DESC);
            CREATE INDEX {table}_mix ON public.{table} (a COLLATE "C", d DESC NULLS LAST) INCLUDE (e) WITH (fillfactor = 70);"#
        ))
        .await
        .expect("建表");
    let snapshot = super::ddl_read::load_postgres(&client, "rillsql", "public", &table).await;
    let _ = client.batch_execute(&format!("DROP TABLE public.{table}")).await;
    let snapshot = snapshot.expect("读取结构");
    let find = |suffix: &str| snapshot.indexes.iter().find(|index| index.name == format!("{table}{suffix}")).unwrap_or_else(|| panic!("{suffix}"));
    let plain = find("_plain");
    assert_eq!(plain.columns.iter().map(|column| (column.name.as_str(), column.descending)).collect::<Vec<_>>(), vec![("a", false), ("B c", true)]);
    assert!(plain.rebuild_loses.is_empty(), "{:?}", plain.rebuild_loses);
    assert_eq!(plain.method.as_deref(), Some("btree"));
    let gin = find("_g");
    assert_eq!(gin.rebuild_loses, vec!["USING gin"]);
    let mix = find("_mix");
    assert_eq!(mix.columns.iter().map(|column| (column.name.as_str(), column.descending)).collect::<Vec<_>>(), vec![("a", false), ("d", true)], "INCLUDE 列不算键列");
    assert_eq!(mix.rebuild_loses, vec!["COLLATE", "NULLS LAST", "INCLUDE", "WITH"]);
    assert!(find("_pkey").rebuild_loses.is_empty());
}
