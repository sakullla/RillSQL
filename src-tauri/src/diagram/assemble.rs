use crate::diagram::model::{
    Diagram, DiagramEdge, DiagramTable, KeyPart, OmittedTable, OutgoingKey, RangeScope, TableAttempt,
};

pub(crate) fn group_key_parts(mut parts: Vec<KeyPart>) -> Vec<OutgoingKey> {
    parts.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then(left.ref_schema.cmp(&right.ref_schema))
            .then(left.ref_table.cmp(&right.ref_table))
            .then(left.ordinal.cmp(&right.ordinal))
    });
    let mut keys: Vec<OutgoingKey> = Vec::new();
    for part in parts {
        if let Some(key) = keys.iter_mut().find(|key| {
            key.name == part.name && key.ref_schema == part.ref_schema && key.ref_table == part.ref_table
        }) {
            key.columns.push(part.column);
            key.ref_columns.push(part.ref_column);
        } else {
            keys.push(OutgoingKey {
                name: part.name,
                columns: vec![part.column],
                ref_schema: part.ref_schema,
                ref_table: part.ref_table,
                ref_columns: vec![part.ref_column],
            });
        }
    }
    keys
}

pub(crate) fn assemble_diagram(scope: &RangeScope, attempts: Vec<TableAttempt>) -> Diagram {
    if attempts.is_empty() {
        return Diagram {
            database: scope.database.clone(),
            schema: scope.schema.clone(),
            tables: Vec::new(),
            edges: Vec::new(),
            omitted: Vec::new(),
            notice: Some("当前范围没有表".to_owned()),
        };
    }

    let mut tables = Vec::new();
    let mut omitted = Vec::new();
    let mut outgoing = Vec::new();
    for attempt in attempts {
        match (attempt.columns, attempt.foreign_keys) {
            (Ok(columns), Ok(keys)) => {
                outgoing.push((attempt.name.clone(), keys));
                tables.push(DiagramTable {
                    name: attempt.name,
                    columns,
                    outside: Vec::new(),
                });
            }
            (Err(reason), _) | (_, Err(reason)) => omitted.push(OmittedTable {
                name: attempt.name.clone(),
                message: format!("没有读到表 {}：{reason}", attempt.name),
            }),
        }
    }

    let mut edges = Vec::new();
    for (from_table, keys) in outgoing {
        for key in keys {
            if !same_scope(scope, &key) {
                push_outside(&mut tables, &from_table, &outside_note(&key));
                continue;
            }
            if omitted.iter().any(|item| names_equal(&item.name, &key.ref_table, scope.fold_names)) {
                continue;
            }
            let Some(to_table) = find_name(tables.iter().map(|table| table.name.as_str()), &key.ref_table, scope.fold_names) else {
                push_outside(&mut tables, &from_table, &outside_note(&key));
                continue;
            };
            edges.push(DiagramEdge {
                name: key.name,
                from_table: from_table.clone(),
                from_columns: key.columns,
                to_table: to_table.to_owned(),
                to_columns: key.ref_columns,
            });
        }
    }

    tables.sort_by(|left, right| left.name.cmp(&right.name));
    omitted.sort_by(|left, right| left.name.cmp(&right.name));
    edges.sort_by(|left, right| {
        left.from_table
            .cmp(&right.from_table)
            .then(left.to_table.cmp(&right.to_table))
            .then(left.name.cmp(&right.name))
            .then(left.from_columns.cmp(&right.from_columns))
    });
    for table in &mut tables {
        table.outside.sort();
    }

    Diagram {
        database: scope.database.clone(),
        schema: scope.schema.clone(),
        tables,
        edges,
        omitted,
        notice: None,
    }
}

fn same_scope(scope: &RangeScope, key: &OutgoingKey) -> bool {
    if scope.schema.is_empty() {
        key.ref_schema.is_empty() || names_equal(&key.ref_schema, &scope.database, scope.fold_names)
    } else {
        let schema = if key.ref_schema.is_empty() { scope.schema.as_str() } else { key.ref_schema.as_str() };
        names_equal(schema, &scope.schema, false)
    }
}

fn find_name<'a>(names: impl Iterator<Item = &'a str>, wanted: &str, fold: bool) -> Option<&'a str> {
    names.into_iter().find(|name| names_equal(name, wanted, fold))
}

fn names_equal(left: &str, right: &str, fold: bool) -> bool {
    if fold {
        left.eq_ignore_ascii_case(right)
    } else {
        left == right
    }
}

fn push_outside(tables: &mut [DiagramTable], from_table: &str, note: &str) {
    if let Some(table) = tables.iter_mut().find(|table| table.name == from_table) {
        if !table.outside.iter().any(|item| item == note) {
            table.outside.push(note.to_owned());
        }
    }
}

fn outside_note(key: &OutgoingKey) -> String {
    let columns = key.ref_columns.join(", ");
    let target = if key.ref_schema.is_empty() {
        format!("{}({columns})", key.ref_table)
    } else {
        format!("{}.{}({columns})", key.ref_schema, key.ref_table)
    };
    if key.name.is_empty() {
        format!("外键指向 {target}，不在当前范围")
    } else {
        format!("外键 {} 指向 {target}，不在当前范围", key.name)
    }
}
