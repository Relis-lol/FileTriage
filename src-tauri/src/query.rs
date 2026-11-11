//! Abfragen auf dem Index: Triage-Liste, Suche, grosse und alte Dateien.
//!
//! Sortierspalten und Richtungen sind fest verdrahtet; alle Werte gehen als
//! gebundene Parameter in die Abfrage. Damit ist keine SQL-Injection moeglich,
//! auch wenn die Oberflaeche einmal ungeprueften Text durchreicht.

use rusqlite::{Connection, ToSql, types::Value};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::model::FileEntry;
use crate::paths;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct FileQuery {
    /// Wurzelordner; leer bedeutet "alle indexierten Ordner".
    pub root: Option<String>,
    /// Freitext auf Name und Pfad.
    pub text: Option<String>,
    pub categories: Vec<String>,
    pub exts: Vec<String>,
    pub states: Vec<String>,
    pub min_size: Option<i64>,
    pub max_size: Option<i64>,
    /// Unix-Sekunden.
    pub modified_before: Option<i64>,
    pub modified_after: Option<i64>,
    /// "name" | "size" | "modified" | "created" | "ext" | "category" | "parent"
    pub sort: String,
    pub desc: bool,
    pub limit: i64,
    pub offset: i64,
}

impl Default for FileQuery {
    fn default() -> Self {
        FileQuery {
            root: None,
            text: None,
            categories: Vec::new(),
            exts: Vec::new(),
            states: Vec::new(),
            min_size: None,
            max_size: None,
            modified_before: None,
            modified_after: None,
            sort: "modified".into(),
            desc: true,
            limit: 200,
            offset: 0,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct FilePage {
    pub items: Vec<FileEntry>,
    pub total: i64,
    pub total_size: i64,
}

fn sort_column(sort: &str) -> &'static str {
    match sort {
        "name" => "name_lower",
        "size" => "size",
        "created" => "created",
        "ext" => "ext",
        "category" => "category",
        "parent" => "parent",
        _ => "modified",
    }
}

/// Baut WHERE-Klausel und Parameterliste.
fn where_clause(q: &FileQuery) -> (String, Vec<Value>) {
    let mut cond: Vec<String> = vec!["1 = 1".into()];
    let mut args: Vec<Value> = Vec::new();

    if let Some(root) = q.root.as_ref().filter(|r| !r.trim().is_empty()) {
        // Wurzel oder Unterordner - ueber den normalisierten Schluessel.
        let key = paths::path_key(root);
        cond.push("(lower(replace(parent, '/', '\\')) = ?  OR lower(replace(parent, '/', '\\')) LIKE ?)".into());
        args.push(Value::Text(key.clone()));
        args.push(Value::Text(format!("{key}\\%")));
    }
    if let Some(text) = q.text.as_ref().filter(|t| !t.trim().is_empty()) {
        let pattern = format!("%{}%", text.trim().to_lowercase());
        cond.push("(name_lower LIKE ? OR lower(path) LIKE ?)".into());
        args.push(Value::Text(pattern.clone()));
        args.push(Value::Text(pattern));
    }
    for (field, values) in [
        ("category", &q.categories),
        ("ext", &q.exts),
        ("state", &q.states),
    ] {
        if values.is_empty() {
            continue;
        }
        let holes = vec!["?"; values.len()].join(",");
        cond.push(format!("{field} IN ({holes})"));
        for v in values {
            args.push(Value::Text(v.to_lowercase()));
        }
    }
    if let Some(v) = q.min_size {
        cond.push("size >= ?".into());
        args.push(Value::Integer(v));
    }
    if let Some(v) = q.max_size {
        cond.push("size <= ?".into());
        args.push(Value::Integer(v));
    }
    if let Some(v) = q.modified_before {
        cond.push("modified <= ? AND modified > 0".into());
        args.push(Value::Integer(v));
    }
    if let Some(v) = q.modified_after {
        cond.push("modified >= ?".into());
        args.push(Value::Integer(v));
    }
    (cond.join(" AND "), args)
}

pub fn list_files(conn: &Connection, q: &FileQuery) -> Result<FilePage> {
    let (where_sql, args) = where_clause(q);
    let dir = if q.desc { "DESC" } else { "ASC" };
    let limit = q.limit.clamp(1, 5_000);
    let offset = q.offset.max(0);

    let sql = format!(
        "SELECT id, path, parent, name, ext, size, created, modified, category, state
         FROM files WHERE {where_sql}
         ORDER BY {} {dir}, id ASC LIMIT ? OFFSET ?",
        sort_column(&q.sort)
    );

    let mut params: Vec<&dyn ToSql> = args.iter().map(|v| v as &dyn ToSql).collect();
    let lim = Value::Integer(limit);
    let off = Value::Integer(offset);
    params.push(&lim);
    params.push(&off);

    let mut stmt = conn.prepare(&sql)?;
    let items = stmt
        .query_map(params.as_slice(), |row| {
            Ok(FileEntry {
                id: row.get(0)?,
                path: row.get(1)?,
                parent: row.get(2)?,
                name: row.get(3)?,
                ext: row.get(4)?,
                size: row.get(5)?,
                created: row.get(6)?,
                modified: row.get(7)?,
                category: row.get(8)?,
                state: row.get(9)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let count_sql = format!("SELECT count(*), coalesce(sum(size), 0) FROM files WHERE {where_sql}");
    let count_params: Vec<&dyn ToSql> = args.iter().map(|v| v as &dyn ToSql).collect();
    let (total, total_size): (i64, i64) = conn.query_row(&count_sql, count_params.as_slice(), |r| {
        Ok((r.get(0)?, r.get(1)?))
    })?;

    Ok(FilePage { items, total, total_size })
}

/// Einzelner Eintrag ueber seine ID.
pub fn file_by_id(conn: &Connection, id: i64) -> Result<Option<FileEntry>> {
    let mut stmt = conn.prepare(
        "SELECT id, path, parent, name, ext, size, created, modified, category, state
         FROM files WHERE id = ?1",
    )?;
    let mut rows = stmt.query([id])?;
    match rows.next()? {
        Some(row) => Ok(Some(FileEntry {
            id: row.get(0)?,
            path: row.get(1)?,
            parent: row.get(2)?,
            name: row.get(3)?,
            ext: row.get(4)?,
            size: row.get(5)?,
            created: row.get(6)?,
            modified: row.get(7)?,
            category: row.get(8)?,
            state: row.get(9)?,
        })),
        None => Ok(None),
    }
}

/// Einzelner Eintrag ueber seinen normalisierten Pfad.
pub fn file_by_path(conn: &Connection, path_key: &str) -> Result<Option<FileEntry>> {
    let mut stmt = conn.prepare(
        "SELECT id, path, parent, name, ext, size, created, modified, category, state
         FROM files WHERE path_key = ?1",
    )?;
    let mut rows = stmt.query([path_key])?;
    match rows.next()? {
        Some(row) => Ok(Some(FileEntry {
            id: row.get(0)?,
            path: row.get(1)?,
            parent: row.get(2)?,
            name: row.get(3)?,
            ext: row.get(4)?,
            size: row.get(5)?,
            created: row.get(6)?,
            modified: row.get(7)?,
            category: row.get(8)?,
            state: row.get(9)?,
        })),
        None => Ok(None),
    }
}

/// Verteilung nach Kategorie fuer die Downloads-Ansicht.
#[derive(Debug, Serialize)]
pub struct CategoryCount {
    pub category: String,
    pub count: i64,
    pub size: i64,
}

pub fn category_breakdown(conn: &Connection, q: &FileQuery) -> Result<Vec<CategoryCount>> {
    let (where_sql, args) = where_clause(q);
    let sql = format!(
        "SELECT category, count(*), coalesce(sum(size),0) FROM files WHERE {where_sql}
         GROUP BY category ORDER BY 3 DESC"
    );
    let params: Vec<&dyn ToSql> = args.iter().map(|v| v as &dyn ToSql).collect();
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params.as_slice(), |r| {
            Ok(CategoryCount { category: r.get(0)?, count: r.get(1)?, size: r.get(2)? })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ScanSettings;
    use crate::db::Db;
    use crate::scan::{ScanOptions, scan};
    use crate::testutil::TempDir;
    use std::sync::atomic::AtomicBool;

    fn vorbereiten() -> (TempDir, Db) {
        let dir = TempDir::new("query");
        dir.file("rechnung-2026.pdf", &vec![0u8; 2048]);
        dir.file("urlaub.jpg", &vec![0u8; 5000]);
        dir.file("unter/setup.exe", &vec![0u8; 100]);
        let db = Db::open(dir.path().join(".idx.db")).unwrap();
        let opts = ScanOptions { root: dir.path().to_path_buf(), settings: ScanSettings::default() };
        scan(&db, &opts, &AtomicBool::new(false), &mut |_| {}).unwrap();
        (dir, db)
    }

    #[test]
    fn textsuche_findet_teiltreffer_ohne_ruecksicht_auf_gross_klein() {
        let (_d, db) = vorbereiten();
        let conn = db.conn().unwrap();
        let q = FileQuery { text: Some("RECHNUNG".into()), ..Default::default() };
        let page = list_files(&conn, &q).unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.items[0].name, "rechnung-2026.pdf");
    }

    #[test]
    fn kategorie_und_groessenfilter_wirken() {
        let (_d, db) = vorbereiten();
        let conn = db.conn().unwrap();
        let q = FileQuery {
            categories: vec!["image".into()],
            min_size: Some(1000),
            ..Default::default()
        };
        let page = list_files(&conn, &q).unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.items[0].category, "image");
    }

    #[test]
    fn sortierung_nach_groesse_absteigend() {
        let (_d, db) = vorbereiten();
        let conn = db.conn().unwrap();
        let q = FileQuery { sort: "size".into(), desc: true, ..Default::default() };
        let page = list_files(&conn, &q).unwrap();
        let sizes: Vec<i64> = page.items.iter().map(|f| f.size).collect();
        let mut sortiert = sizes.clone();
        sortiert.sort_unstable_by(|a, b| b.cmp(a));
        assert_eq!(sizes, sortiert);
    }

    #[test]
    fn unbekannte_sortierspalte_faellt_auf_standard_zurueck() {
        assert_eq!(sort_column("size); DROP TABLE files;--"), "modified");
    }

    #[test]
    fn kategorieuebersicht_summiert() {
        let (_d, db) = vorbereiten();
        let conn = db.conn().unwrap();
        let breakdown = category_breakdown(&conn, &FileQuery::default()).unwrap();
        assert!(breakdown.iter().any(|c| c.category == "pdf" && c.count == 1));
    }
}
