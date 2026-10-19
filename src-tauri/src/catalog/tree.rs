use std::collections::HashSet;

use crate::catalog::model::{
    kind_name, ConnectionNode, DatabaseNode, ObjectKind, ObjectRow, SchemaNode, DEFAULT_OBJECT_PAGE,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectPage {
    pub items: Vec<ObjectRow>,
    pub total: usize,
    pub has_more: bool,
}

pub fn take_page(objects: &[ObjectRow], offset: usize, limit: usize) -> ObjectPage {
    let limit = if limit == 0 {
        DEFAULT_OBJECT_PAGE
    } else {
        limit
    };
    let items: Vec<ObjectRow> = objects.iter().skip(offset).take(limit).cloned().collect();
    let shown = offset + items.len();
    ObjectPage {
        has_more: shown < objects.len(),
        total: objects.len(),
        items,
    }
}

pub fn object_path(connection: &str, database: &str, schema: &str) -> String {
    [connection, database, schema]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" / ")
}

pub fn object_key(object: &ObjectRow) -> String {
    format!(
        "{}\u{1}{}\u{1}{}\u{1}{}",
        kind_name(object.kind),
        object.name,
        object.signature,
        object.relation
    )
}

/// 第一次找不到的对象标为失效；已经失效的对象在下一次刷新时移除。
pub fn reconcile_objects(previous: &[ObjectRow], incoming: &[ObjectRow]) -> Vec<ObjectRow> {
    let mut objects = Vec::new();
    let mut seen = HashSet::new();
    let incoming_keys: HashSet<String> = incoming.iter().map(object_key).collect();
    for old in previous {
        let key = object_key(old);
        seen.insert(key.clone());
        if old.stale {
            continue;
        }
        match incoming.iter().find(|item| object_key(item) == key) {
            Some(fresh) => {
                let mut row = fresh.clone();
                row.stale = false;
                objects.push(row);
            }
            None => {
                let mut row = old.clone();
                row.stale = true;
                objects.push(row);
            }
        }
    }
    for item in incoming {
        if !seen.contains(&object_key(item)) && incoming_keys.contains(&object_key(item)) {
            let mut row = item.clone();
            row.stale = false;
            objects.push(row);
        }
    }
    objects
}

fn failed_kinds(schema: &SchemaNode) -> HashSet<ObjectKind> {
    schema
        .kind_reports
        .iter()
        .filter(|report| report.error.is_some())
        .map(|report| report.kind)
        .collect()
}

pub fn refresh_schema(previous: &SchemaNode, incoming: &SchemaNode) -> SchemaNode {
    if let Some(message) = &incoming.error {
        return SchemaNode {
            name: if incoming.name.is_empty() {
                previous.name.clone()
            } else {
                incoming.name.clone()
            },
            objects: previous.objects.clone(),
            total: previous.objects.len(),
            error: Some(message.clone()),
            kind_reports: incoming.kind_reports.clone(),
        };
    }
    let failed = failed_kinds(incoming);
    let kept: Vec<ObjectRow> = previous
        .objects
        .iter()
        .filter(|item| failed.contains(&item.kind))
        .cloned()
        .collect();
    let previous_open: Vec<ObjectRow> = previous
        .objects
        .iter()
        .filter(|item| !failed.contains(&item.kind))
        .cloned()
        .collect();
    let incoming_open: Vec<ObjectRow> = incoming
        .objects
        .iter()
        .filter(|item| !failed.contains(&item.kind))
        .cloned()
        .collect();
    let mut objects = reconcile_objects(&previous_open, &incoming_open);
    let kept_len = kept.len();
    objects.extend(kept);
    SchemaNode {
        name: incoming.name.clone(),
        total: incoming.total.saturating_add(kept_len).max(objects.len()),
        objects,
        error: None,
        kind_reports: incoming.kind_reports.clone(),
    }
}

pub fn refresh_connection(previous: &ConnectionNode, incoming: &ConnectionNode) -> ConnectionNode {
    if incoming.error.is_some() && incoming.databases.is_empty() {
        return ConnectionNode {
            id: incoming.id.clone(),
            name: incoming.name.clone(),
            engine: incoming.engine,
            status: incoming.status,
            foreign_keys: if incoming.engine.shows_foreign_keys() {
                incoming.foreign_keys.or(previous.foreign_keys)
            } else {
                None
            },
            databases: previous.databases.clone(),
            error: incoming.error.clone(),
        };
    }
    let mut databases = Vec::new();
    for database in &incoming.databases {
        let Some(old_database) = previous
            .databases
            .iter()
            .find(|item| item.name == database.name)
        else {
            databases.push(database.clone());
            continue;
        };
        if database.error.is_some() {
            let mut kept = old_database.clone();
            kept.error = database.error.clone();
            databases.push(kept);
            continue;
        }
        let mut schemas = Vec::new();
        for schema in &database.schemas {
            match old_database
                .schemas
                .iter()
                .find(|item| item.name == schema.name)
            {
                Some(old) => schemas.push(refresh_schema(old, schema)),
                None => schemas.push(schema.clone()),
            }
        }
        databases.push(DatabaseNode {
            name: database.name.clone(),
            schemas,
            error: None,
        });
    }
    ConnectionNode {
        id: incoming.id.clone(),
        name: incoming.name.clone(),
        engine: incoming.engine,
        status: incoming.status,
        foreign_keys: if incoming.engine.shows_foreign_keys() {
            incoming.foreign_keys
        } else {
            None
        },
        databases,
        error: incoming.error.clone(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub name: String,
    pub path: String,
    pub stale: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchOutcome {
    pub hits: Vec<SearchHit>,
    pub skipped: Vec<String>,
}

pub fn search_objects(connections: &[ConnectionNode], query: &str) -> SearchOutcome {
    let needle = query.trim().to_lowercase();
    let mut hits = Vec::new();
    let mut skipped = Vec::new();
    for connection in connections {
        if !connection.status.is_connected() {
            skipped.push(connection.name.clone());
            continue;
        }
        if needle.is_empty() {
            continue;
        }
        for database in &connection.databases {
            if database.error.is_some() {
                continue;
            }
            for schema in &database.schemas {
                if schema.error.is_some() && schema.objects.is_empty() {
                    continue;
                }
                for object in &schema.objects {
                    if !object.name.to_lowercase().contains(&needle) {
                        continue;
                    }
                    hits.push(SearchHit {
                        name: object.name.clone(),
                        path: object_path(&connection.name, &database.name, &schema.name),
                        stale: object.stale,
                    });
                }
            }
        }
    }
    SearchOutcome { hits, skipped }
}

pub fn visible_database_names(connection: &ConnectionNode) -> Vec<String> {
    if !connection.engine.shows_databases() || !connection.status.is_connected() {
        return Vec::new();
    }
    connection
        .databases
        .iter()
        .map(|database| database.name.clone())
        .collect()
}

pub fn visible_schema_names(connection: &ConnectionNode) -> Vec<String> {
    if !connection.engine.shows_schemas() || !connection.status.is_connected() {
        return Vec::new();
    }
    connection
        .databases
        .iter()
        .flat_map(|database| {
            database
                .schemas
                .iter()
                .filter(|schema| !schema.name.is_empty())
                .map(|schema| schema.name.clone())
        })
        .collect()
}

pub fn direct_objects(connection: &ConnectionNode) -> Vec<ObjectRow> {
    if connection.engine.shows_databases() || !connection.status.is_connected() {
        return Vec::new();
    }
    connection
        .databases
        .iter()
        .flat_map(|database| {
            database
                .schemas
                .iter()
                .flat_map(|schema| schema.objects.clone())
        })
        .collect()
}
