//! MongoDB 文档通路核心（R12）：JSON 过滤解析、文档 JSON 化、分页/计数与 CRUD 组装。
//! JSON 过滤的语法错误先经 serde_json 拿到行列定位，再经 bson 校验扩展 JSON 语义（$oid/$date 等）。

use futures::StreamExt;
use mongodb::bson::{Bson, Document};

/// 文档分页默认页大小（与网格默认行数一致）。
pub const DEFAULT_PAGE_LIMIT: u64 = 100;

/// 一页文档。id_json 是 _id 的扩展 JSON 文本（前端定位/替换/删除都用它），json 是整档文档。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MongoDoc {
    pub id_json: String,
    pub json: String,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentsPage {
    pub total: i64,
    pub documents: Vec<MongoDoc>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseSummary {
    pub name: String,
    pub collections: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectionSummary {
    pub name: String,
    pub estimated_documents: Option<i64>,
}

/// 把 JSON 文本解析成 BSON 文档。
/// 语法错误（括号/引号/逗号）来自 serde_json，消息自带“at line L column C”定位；
/// 语法正确但扩展 JSON 语义非法（如 {"$numberLong": 5}）来自 bson 转换，消息给出原因。
/// subject 是错误文案的主体词（调用方传入）：过滤用“过滤”，文档编辑/新增用“文档”。
pub fn parse_document(subject: &str, text: &str) -> Result<Document, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(format!("{subject}为空：请输入 {{\"字段\": 值}} 形式的 JSON 对象。"));
    }
    let value: serde_json::Value = serde_json::from_str(trimmed).map_err(|err| format!("{subject}语法错误：{err}"))?;
    if !value.is_object() {
        return Err(format!("{subject}应是 JSON 对象（{{\"字段\": 值}}），不是 {kind}。", kind = json_kind(&value)));
    }
    let bson: Bson = Bson::try_from(value).map_err(|err| format!("{subject}里的扩展 JSON 值不合法：{err}"))?;
    match bson {
        Bson::Document(document) => Ok(document),
        other => Err(format!("{subject}应是 JSON 对象，解析成了 {other:?}。")),
    }
}

/// 解析 _id 的扩展 JSON 文本（单值或 {"$oid": ...} 对象）。
pub fn parse_id(text: &str) -> Result<Bson, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("缺少文档 _id。".to_owned());
    }
    let value: serde_json::Value = serde_json::from_str(trimmed).map_err(|err| format!("_id 不是合法 JSON：{err}"))?;
    Bson::try_from(value).map_err(|err| format!("_id 不是合法的扩展 JSON 值：{err}"))
}

fn json_kind(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "布尔值",
        serde_json::Value::Number(_) => "数字",
        serde_json::Value::String(_) => "字符串",
        serde_json::Value::Array(_) => "数组",
        serde_json::Value::Object(_) => "对象",
    }
}

/// 删除文档前的确认。未确认时不得发出 delete。
pub fn require_delete(confirmed: bool) -> Result<(), String> {
    if !confirmed {
        return Err("删除文档属于写删操作，需要先在界面确认。".to_owned());
    }
    Ok(())
}

/// 按 _id 定位的过滤文档。
pub fn id_filter(id: Bson) -> Document {
    mongodb::bson::doc! { "_id": id }
}

/// BSON 文档 → relaxed 扩展 JSON 文本（ObjectId→{"$oid"}、日期→{"$date"}，数字保持字面）。
/// relaxed 形态可直接回读（编辑/替换往返），也便于人读。
pub fn document_to_json(document: &Document) -> String {
    let relaxed: serde_json::Value = Bson::Document(document.clone()).into_relaxed_extjson();
    serde_json::to_string(&relaxed).unwrap_or_else(|_| "{}".to_owned())
}

/// BSON 值 → relaxed 扩展 JSON 文本（_id 列等单值展示）。
pub fn value_to_json(value: &Bson) -> String {
    let relaxed: serde_json::Value = value.clone().into_relaxed_extjson();
    serde_json::to_string(&relaxed).unwrap_or_else(|_| "null".to_owned())
}

/// 文档计数策略（与 Compass 一致）：无过滤用 estimatedDocumentCount（快，含未落盘估算）；
/// 有过滤用 count_documents（精确匹配过滤条件）。
pub fn count_mode(filter: &Document) -> CountMode {
    if filter.is_empty() {
        CountMode::Estimated
    } else {
        CountMode::Exact
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CountMode {
    Estimated,
    Exact,
}

/// 库清单里不显示服务器管理库（admin/config/local），目录树与库下拉共用这条规则。
pub(crate) fn catalog_hidden(name: &str) -> bool {
    matches!(name, "admin" | "config" | "local")
}

/// 库清单：不显示 admin/config/local（目录树同规则）。
pub async fn list_databases(client: &mongodb::Client) -> Result<Vec<DatabaseSummary>, String> {
    let mut names = client.list_database_names().await.map_err(|err| err.to_string())?;
    names.retain(|name| !catalog_hidden(name));
    names.sort();
    let mut out = Vec::with_capacity(names.len());
    for name in names {
        let collections = client
            .database(&name)
            .list_collection_names()
            .await
            .map_err(|err| err.to_string())?
            .len();
        out.push(DatabaseSummary { name, collections });
    }
    Ok(out)
}

/// 集合清单 + estimatedDocumentCount 计数。
pub async fn list_collections(client: &mongodb::Client, database: &str) -> Result<Vec<CollectionSummary>, String> {
    let mut names = client
        .database(database)
        .list_collection_names()
        .await
        .map_err(|err| err.to_string())?;
    names.sort();
    let mut out = Vec::with_capacity(names.len());
    for name in names {
        let estimated = client
            .database(database)
            .collection::<Document>(&name)
            .estimated_document_count()
            .await
            .ok()
            .and_then(|count| i64::try_from(count).ok());
        out.push(CollectionSummary {
            name,
            estimated_documents: estimated,
        });
    }
    Ok(out)
}

/// 分页读取文档：skip/limit + 可选过滤，按 _id 升序保证翻页稳定。
/// total 按 count_mode 给出（estimated 或精确）。
pub async fn find_documents(
    client: &mongodb::Client,
    database: &str,
    collection: &str,
    skip: u64,
    limit: u64,
    filter: Document,
) -> mongodb::error::Result<DocumentsPage> {
    let handle = client.database(database).collection::<Document>(collection);
    let total = match count_mode(&filter) {
        CountMode::Estimated => handle.estimated_document_count().await,
        CountMode::Exact => handle.count_documents(filter.clone()).await,
    }?;
    let mut cursor = handle.find(filter).sort(mongodb::bson::doc! { "_id": 1 }).skip(skip).limit(i64::try_from(limit).unwrap_or(100)).await?;
    let mut documents = Vec::new();
    while let Some(document) = cursor.next().await {
        let document = document?;
        let id = document.get("_id").cloned().unwrap_or(Bson::Null);
        documents.push(MongoDoc {
            id_json: value_to_json(&id),
            json: document_to_json(&document),
        });
    }
    Ok(DocumentsPage {
        total: i64::try_from(total).unwrap_or(i64::MAX),
        documents,
    })
}

/// 读单个文档（按 _id），返回整档 relaxed JSON 文本；None = 文档不存在。
pub async fn read_document(
    client: &mongodb::Client,
    database: &str,
    collection: &str,
    id: Bson,
) -> mongodb::error::Result<Option<String>> {
    let handle = client.database(database).collection::<Document>(collection);
    let found = handle.find_one(id_filter(id)).await?;
    Ok(found.as_ref().map(document_to_json))
}

#[cfg(test)]
mod doc_tests {
    use super::*;

    #[test]
    fn parse_document_reports_json_syntax_location() {
        let error = parse_document("过滤", "{\"status\": }").unwrap_err();
        assert!(error.contains("line"), "语法错误要带行号定位：{error}");
        assert!(error.contains("column"), "语法错误要带列号定位：{error}");
        assert!(error.starts_with("过滤语法错误"), "{error}");
        // 文档编辑/新增共用同一解析器：主体词换成“文档”。
        let doc_error = parse_document("文档", "{\"status\": }").unwrap_err();
        assert!(doc_error.starts_with("文档语法错误"), "{doc_error}");
    }

    #[test]
    fn parse_document_accepts_relaxed_and_extended_values() {
        let doc = parse_document("过滤", "{\"status\":\"active\",\"count\":{\"$numberLong\":\"5\"}}").unwrap();
        assert_eq!(doc.get_str("status"), Ok("active"));
        assert_eq!(doc.get_i64("count"), Ok(5));
    }

    #[test]
    fn parse_document_rejects_invalid_extended_json_with_reason() {
        let error = parse_document("文档", "{\"count\":{\"$numberLong\":5}}").unwrap_err();
        assert!(error.contains("扩展 JSON"), "{error}");
        assert!(error.starts_with("文档里的扩展 JSON"), "{error}");
    }

    #[test]
    fn parse_document_requires_object() {
        let error = parse_document("文档", "[1,2]").unwrap_err();
        assert!(error.contains("JSON 对象"), "{error}");
        assert!(error.starts_with("文档应是"), "{error}");
        assert!(parse_document("过滤", "").is_err());
        assert!(parse_document("文档", "  ").unwrap_err().starts_with("文档为空"));
    }

    #[test]
    fn parse_id_roundtrips_objectid_and_string() {
        let oid = parse_id("{\"$oid\":\"65f2c1e4a1b2c3d4e5f60718\"}").unwrap();
        assert!(matches!(oid, Bson::ObjectId(_)));
        let text = parse_id("\"orders-1001\"").unwrap();
        assert_eq!(text.as_str(), Some("orders-1001"));
        assert!(parse_id("not-json").is_err());
    }

    #[test]
    fn document_to_json_uses_relaxed_extended_form() {
        let doc = mongodb::bson::doc! {
            "_id": mongodb::bson::oid::ObjectId::from_bytes([0x65, 0xf2, 0xc1, 0xe4, 0xa1, 0xb2, 0xc3, 0xd4, 0xe5, 0xf6, 0x07, 0x18]),
            "数量": 3,
            "标签": ["加急", "普通"],
            "地址": { "城市": "杭州" },
        };
        let json = document_to_json(&doc);
        assert!(json.contains("\"$oid\":\"65f2c1e4a1b2c3d4e5f60718\""), "{json}");
        assert!(json.contains("\"数量\":3"), "{json}");
        assert!(json.contains("加急"), "{json}");
        assert!(json.contains("杭州"), "{json}");
        // 回读：编辑流程提交的正是这个形态。
        let back = parse_document("文档", &json).unwrap();
        assert_eq!(back.get_array("标签").map(Vec::len), Ok(2));
        assert_eq!(back.get_document("地址").map(|d| d.get_str("城市")), Ok(Ok("杭州")));
    }

    #[test]
    fn count_mode_is_estimated_only_without_filter() {
        assert_eq!(count_mode(&Document::new()), CountMode::Estimated);
        assert_eq!(count_mode(&mongodb::bson::doc! {"status": "active"}), CountMode::Exact);
    }
}
