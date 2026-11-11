//! Regeln und Vorschlaege.
//!
//! Zwei getrennte Dinge, die oft verwechselt werden:
//!
//! - Eine **Regel** ist etwas, das der Nutzer angelegt oder bestaetigt hat. Sie
//!   ordnet passenden Dateien einen Zielordner zu - aber sie fuehrt nichts von
//!   selbst aus. Ausgefuehrt wird nur, was der Nutzer nach einem Dry Run startet.
//! - Ein **Vorschlag** entsteht aus beobachtetem Verhalten ("du hast schon
//!   dreimal PDFs mit 'rechnung' hierhin geschoben"). Er wird erst zur Regel,
//!   wenn der Nutzer zustimmt.

use std::path::Path;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::db;
use crate::error::{AppError, Result};
use crate::fileops::{self, CollisionPolicy, OpResult};
use crate::model::FileEntry;
use crate::query::{self, FileQuery};

/// Eine einzelne Bedingung.
///
/// Fuer Dateinamen gilt durchgehend:
///
/// * **Gross- und Kleinschreibung ist egal.** Windows-Dateinamen sind es auch.
///   Verglichen wird beidseitig in Kleinbuchstaben, Unicode-korrekt - also auch
///   fuer `UE`, `E` mit Akzent oder koreanische Silben.
/// * **Der Text wird nicht veraendert.** Regeln lesen nur; sie benennen nichts
///   um.
/// * Ein fuehrender Punkt gehoert bei Namensbedingungen zum Muster
///   (`.backup` sucht wirklich nach `.backup`). Nur bei `ext` wird er
///   abgeschnitten, weil dort `.pdf` und `pdf` dasselbe meinen.
/// * `ends_with` auf `name` vergleicht den Namen **ohne Endung**: `_backup`
///   findet `projekt_backup.zip`. Damit niemand ins Leere greift, passt
///   zusaetzlich der vollstaendige Name - ein eingetipptes `.zip` wirkt also
///   ebenfalls.
/// * Mehrere Werte in einer Bedingung wirken als "eines davon".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Condition {
    /// "ext" | "name" | "path" | "category" | "size" | "age_days"
    pub field: String,
    /// "eq" | "in" | "contains" | "starts_with" | "ends_with" | "gt" | "lt"
    pub op: String,
    /// Text, Zahl oder Liste - je nach Feld.
    pub value: serde_json::Value,
}

/// Wie die Bedingungen einer Regel verknuepft sind.
///
/// Voreinstellung ist `all`. Wer eine Regel anlegt, denkt fast immer an "und":
/// "Rechnungen sind PDFs, die 'rechnung' heissen" - nicht "PDFs oder Dateien
/// mit 'rechnung' im Namen". `any` gibt es fuer den Fall, dass mehrere Woerter
/// dasselbe meinen.
fn standard_verknuepfung() -> String {
    "all".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    #[serde(default)]
    pub id: Option<i64>,
    pub name: String,
    #[serde(default = "wahr")]
    pub enabled: bool,
    #[serde(default = "standard_prioritaet")]
    pub priority: i64,
    pub conditions: Vec<Condition>,
    /// "all" (alle Bedingungen muessen zutreffen) oder "any" (mindestens eine).
    #[serde(default = "standard_verknuepfung")]
    pub match_mode: String,
    /// Zielordner als absoluter Pfad.
    pub target: String,
    /// "user" (selbst angelegt) oder "suggested" (aus einem Vorschlag entstanden).
    #[serde(default = "quelle_user")]
    pub source: String,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub hits: i64,
}

fn wahr() -> bool {
    true
}
fn standard_prioritaet() -> i64 {
    100
}
fn quelle_user() -> String {
    "user".into()
}

impl Rule {
    /// Prueft, ob die Regel auf eine Datei passt.
    pub fn matches(&self, file: &FileEntry, now: i64) -> bool {
        if !self.enabled || self.conditions.is_empty() {
            return false;
        }
        if self.match_mode == "any" {
            self.conditions.iter().any(|c| bedingung_passt(c, file, now))
        } else {
            self.conditions.iter().all(|c| bedingung_passt(c, file, now))
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            return Err(AppError::Invalid("Die Regel braucht einen Namen".into()));
        }
        if self.conditions.is_empty() {
            return Err(AppError::Invalid(
                "Ohne Bedingung wuerde die Regel auf alles passen".into(),
            ));
        }
        if !Path::new(&self.target).is_absolute() {
            return Err(AppError::Invalid(format!(
                "Der Zielordner braucht einen absoluten Pfad: {}",
                self.target
            )));
        }
        if !["all", "any"].contains(&self.match_mode.as_str()) {
            return Err(AppError::Invalid(format!(
                "Unbekannte Verknuepfung: {}",
                self.match_mode
            )));
        }
        for c in &self.conditions {
            let ops: &[&str] = match c.field.as_str() {
                "ext" | "category" => &["in", "eq"],
                "name" | "path" => &["contains", "starts_with", "ends_with", "eq"],
                "size" | "age_days" => &["gt", "lt", "eq"],
                _ => return Err(AppError::Invalid(format!("Unbekanntes Feld: {}", c.field))),
            };
            if !ops.contains(&c.op.as_str()) {
                return Err(AppError::Invalid(format!(
                    "Der Vergleich \"{}\" passt nicht zum Feld \"{}\"",
                    c.op, c.field
                )));
            }
            // Eine leere Bedingung wuerde je nach Vergleich auf alles passen.
            // Das ist genau die Art Regel, die einen ganzen Ordner verschiebt,
            // ohne dass jemand es wollte.
            if ["name", "path", "ext", "category"].contains(&c.field.as_str())
                && als_muster(&c.value).is_empty()
            {
                return Err(AppError::Invalid(
                    "Eine Bedingung ohne Text wuerde auf alles passen".into(),
                ));
            }
        }
        Ok(())
    }
}

fn als_texte(value: &serde_json::Value) -> Vec<String> {
    match value {
        serde_json::Value::Array(a) => a
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.trim().trim_start_matches('.').to_lowercase()))
            .collect(),
        serde_json::Value::String(s) => {
            vec![s.trim().trim_start_matches('.').to_lowercase()]
        }
        _ => Vec::new(),
    }
}

/// Werte einer Namens- oder Pfadbedingung.
///
/// Anders als [`als_texte`] bleibt hier ein fuehrender Punkt stehen: Bei einer
/// Endungsbedingung meinen `.pdf` und `pdf` dasselbe, bei einem Namensmuster
/// nicht - `.backup` soll wirklich den Punkt suchen.
fn als_muster(value: &serde_json::Value) -> Vec<String> {
    let eins = |s: &str| {
        let t = s.trim().to_lowercase();
        if t.is_empty() { None } else { Some(t) }
    };
    match value {
        serde_json::Value::Array(a) => a.iter().filter_map(|v| v.as_str().and_then(eins)).collect(),
        serde_json::Value::String(s) => eins(s).into_iter().collect(),
        _ => Vec::new(),
    }
}

/// Dateiname ohne die letzte Endung. `bild.tar.gz` wird zu `bild.tar`.
/// Ein fuehrender Punkt bleibt stehen: `.gitignore` ist kein Stamm mit Endung.
fn stamm(name: &str) -> &str {
    match name.rfind('.') {
        Some(0) | None => name,
        Some(p) => &name[..p],
    }
}

fn als_zahl(value: &serde_json::Value) -> Option<i64> {
    value.as_i64().or_else(|| value.as_f64().map(|f| f as i64))
}

fn bedingung_passt(c: &Condition, file: &FileEntry, now: i64) -> bool {
    match c.field.as_str() {
        "ext" => {
            let werte = als_texte(&c.value);
            let ext = file.ext.to_lowercase();
            match c.op.as_str() {
                "in" | "eq" => werte.contains(&ext),
                _ => false,
            }
        }
        "category" => {
            let werte = als_texte(&c.value);
            match c.op.as_str() {
                "in" | "eq" => werte.contains(&file.category.to_lowercase()),
                _ => false,
            }
        }
        "name" | "path" => {
            let heuhaufen = if c.field == "name" {
                file.name.to_lowercase()
            } else {
                file.path.to_lowercase()
            };
            let nadeln = als_muster(&c.value);
            match c.op.as_str() {
                // Mehrere Werte wirken als "eines davon".
                "contains" => nadeln.iter().any(|n| heuhaufen.contains(n)),
                "starts_with" => nadeln.iter().any(|n| heuhaufen.starts_with(n)),
                // Beim Namen ist der Stamm gemeint - "_backup" findet
                // "projekt_backup.zip". Der volle Name passt zusaetzlich, damit
                // ein eingetipptes ".zip" nicht wirkungslos bleibt.
                "ends_with" => {
                    let ohne_endung = if c.field == "name" { stamm(&heuhaufen) } else { "" };
                    nadeln.iter().any(|n| {
                        heuhaufen.ends_with(n)
                            || (!ohne_endung.is_empty() && ohne_endung.ends_with(n))
                    })
                }
                "eq" => nadeln.contains(&heuhaufen),
                _ => false,
            }
        }
        "size" => match (c.op.as_str(), als_zahl(&c.value)) {
            ("gt", Some(v)) => file.size > v,
            ("lt", Some(v)) => file.size < v,
            ("eq", Some(v)) => file.size == v,
            _ => false,
        },
        "age_days" => {
            let Some(v) = als_zahl(&c.value) else { return false };
            if file.modified <= 0 {
                return false;
            }
            // Saettigende Arithmetik: Ein absurder Zeitstempel - aus einer
            // beschaedigten Datei oder einem fremden Dateisystem - darf die
            // Berechnung nicht ueberlaufen lassen. Im Release-Build wuerde der
            // Ueberlauf stillschweigend umlaufen und die Regel auf die falschen
            // Dateien anwenden.
            let alter = now.saturating_sub(file.modified) / 86_400;
            match c.op.as_str() {
                "gt" => alter > v,
                "lt" => alter < v,
                _ => false,
            }
        }
        _ => false,
    }
}

// --------------------------------------------------------------- Speicher

pub fn list(conn: &Connection) -> Result<Vec<Rule>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, enabled, priority, match_json, target, source, created_at, hits,
                match_mode
         FROM rules ORDER BY priority ASC, id ASC",
    )?;
    let rows = stmt
        .query_map([], |r| {
            let json: String = r.get(4)?;
            Ok(Rule {
                id: Some(r.get(0)?),
                name: r.get(1)?,
                enabled: r.get::<_, i32>(2)? != 0,
                priority: r.get(3)?,
                conditions: serde_json::from_str(&json).unwrap_or_default(),
                target: r.get(5)?,
                source: r.get(6)?,
                created_at: r.get(7)?,
                hits: r.get(8)?,
                match_mode: r.get(9)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn save(conn: &Connection, rule: &Rule) -> Result<i64> {
    // Ein leerer Name ist kein Fehler des Nutzers, sondern eine Luecke, die
    // FileTriage selbst schliessen kann.
    let mut rule = rule.clone();
    if rule.name.trim().is_empty() {
        rule.name = default_name(&rule);
    }
    let rule = &rule;
    rule.validate()?;
    let json = serde_json::to_string(&rule.conditions)?;
    match rule.id {
        Some(id) => {
            let n = conn.execute(
                "UPDATE rules SET name = ?1, enabled = ?2, priority = ?3, match_json = ?4,
                                  target = ?5, match_mode = ?7 WHERE id = ?6",
                rusqlite::params![
                    rule.name,
                    rule.enabled as i32,
                    rule.priority,
                    json,
                    rule.target,
                    id,
                    rule.match_mode
                ],
            )?;
            if n == 0 {
                return Err(AppError::NotFound(format!("Regel {id}")));
            }
            Ok(id)
        }
        None => {
            conn.execute(
                "INSERT INTO rules (name, enabled, priority, match_json, target, source,
                                    created_at, match_mode)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    rule.name,
                    rule.enabled as i32,
                    rule.priority,
                    json,
                    rule.target,
                    rule.source,
                    db::now_ts(),
                    rule.match_mode
                ],
            )?;
            Ok(conn.last_insert_rowid())
        }
    }
}

pub fn delete(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM rules WHERE id = ?1", [id])?;
    Ok(())
}

pub fn by_id(conn: &Connection, id: i64) -> Result<Rule> {
    list(conn)?
        .into_iter()
        .find(|r| r.id == Some(id))
        .ok_or_else(|| AppError::NotFound(format!("Regel {id}")))
}

// ---------------------------------------------------------------- Dry Run

#[derive(Debug, Serialize)]
pub struct DryRun {
    pub rule_id: i64,
    pub rule_name: String,
    pub target: String,
    pub count: usize,
    pub total_size: i64,
    /// Vollstaendige Liste der betroffenen Pfade - der Nutzer soll sehen,
    /// was passieren wuerde, bevor er zustimmt.
    pub files: Vec<FileEntry>,
    /// Wahr, wenn die Betrachtung an der Obergrenze abgeschnitten wurde. Dann
    /// ist `count` eine Untergrenze, kein vollstaendiges Ergebnis - und das
    /// muss die Oberflaeche sagen, statt eine Zahl vorzutaeuschen.
    pub truncated: bool,
    /// Treffer, deren Name im Zielordner bereits existiert. Sie gehen nicht
    /// verloren - sie bekommen beim Verschieben eine Nummer angehaengt -, aber
    /// der Nutzer soll die Zahl vorher sehen.
    pub conflicts: usize,
    /// Treffer ohne Namenskonflikt.
    pub safe: usize,
}

/// So viele Dateien betrachtet ein Dry Run hoechstens.
pub const DRY_RUN_LIMIT: i64 = 5_000;

/// Ermittelt, welche Dateien eine Regel treffen wuerde. Aendert nichts.
pub fn dry_run(conn: &Connection, rule_id: i64, root: Option<&str>) -> Result<DryRun> {
    let rule = by_id(conn, rule_id)?;
    let now = db::now_ts();

    // Grobfilter in SQL, Feinpruefung in Rust - so bleiben die Bedingungen an
    // einer Stelle definiert. Durchlaufen wird seitenweise: Eine Zahl, die nur
    // die erste Seite zaehlt, waere schlimmer als gar keine - der Nutzer wuerde
    // "183 Dateien" bestaetigen und 3.000 bekaeme.
    const SEITE: i64 = 2_000;
    let mut treffer: Vec<FileEntry> = Vec::new();
    let mut offset = 0i64;
    let mut abgeschnitten = false;

    loop {
        let seite = query::list_files(
            conn,
            &FileQuery {
                root: root.map(str::to_string),
                states: vec!["pending".into(), "skipped".into(), "kept".into()],
                sort: "name".into(),
                desc: false,
                limit: SEITE,
                offset,
                ..Default::default()
            },
        )?;
        let gelesen = seite.items.len() as i64;

        treffer.extend(
            seite
                .items
                .into_iter()
                .filter(|f| rule.matches(f, now))
                // Dateien, die schon im Ziel liegen, sind keine Kandidaten.
                .filter(|f| {
                    crate::paths::path_key(&f.parent) != crate::paths::path_key(&rule.target)
                }),
        );

        offset += gelesen;
        if gelesen < SEITE {
            break;
        }
        if treffer.len() as i64 >= DRY_RUN_LIMIT {
            abgeschnitten = true;
            treffer.truncate(DRY_RUN_LIMIT as usize);
            break;
        }
    }

    // Namenskonflikte zaehlen. Geprueft wird gegen die Platte, nicht gegen den
    // Index: Im Zielordner koennen Dateien liegen, die FileTriage nie gesehen
    // hat.
    //
    // Der Zielordner wird dafuer *einmal* gelesen. Die naheliegende Fassung -
    // je Treffer ein `exists()` - kostete bei 3.300 Treffern rund 700 ms und
    // machte die Vorschau spuerbar traege; ein einziger Verzeichnisdurchlauf
    // kostet einen Bruchteil davon.
    let belegt = vorhandene_namen(&rule.target);
    let konflikte = treffer
        .iter()
        .filter(|f| belegt.contains(&f.name.to_lowercase()))
        .count();

    Ok(DryRun {
        rule_id,
        rule_name: rule.name,
        target: rule.target,
        count: treffer.len(),
        total_size: treffer.iter().map(|f| f.size).sum(),
        conflicts: konflikte,
        safe: treffer.len() - konflikte,
        files: treffer,
        truncated: abgeschnitten,
    })
}

/// Alle Namen im Zielordner, kleingeschrieben.
///
/// Existiert der Ordner noch nicht, ist die Menge leer - dann kann es dort
/// auch keinen Konflikt geben.
fn vorhandene_namen(ziel: &str) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    let Ok(eintraege) = std::fs::read_dir(crate::paths::extended(Path::new(ziel))) else {
        return out;
    };
    for e in eintraege.flatten() {
        out.insert(e.file_name().to_string_lossy().to_lowercase());
    }
    out
}

/// Zaehlt nur, wie viele Dateien eine Regel gerade treffen wuerde.
///
/// Dieselbe Auswahl wie [`dry_run`], aber ohne die Treffer im Speicher zu
/// halten und ohne die Platte nach Namenskonflikten zu fragen. Damit kann die
/// Regelliste bei jeder Regel eine ehrliche Trefferzahl zeigen, ohne dass das
/// Oeffnen der Seite spuerbar dauert.
pub fn match_count(conn: &Connection, rule: &Rule, root: Option<&str>) -> Result<(usize, bool)> {
    let now = db::now_ts();
    const SEITE: i64 = 2_000;
    let mut treffer = 0usize;
    let mut offset = 0i64;

    loop {
        let seite = query::list_files(
            conn,
            &FileQuery {
                root: root.map(str::to_string),
                states: vec!["pending".into(), "skipped".into(), "kept".into()],
                sort: "name".into(),
                desc: false,
                limit: SEITE,
                offset,
                ..Default::default()
            },
        )?;
        let gelesen = seite.items.len() as i64;
        treffer += seite
            .items
            .into_iter()
            .filter(|f| rule.matches(f, now))
            .filter(|f| crate::paths::path_key(&f.parent) != crate::paths::path_key(&rule.target))
            .count();
        offset += gelesen;
        if gelesen < SEITE {
            return Ok((treffer, false));
        }
        if treffer as i64 >= DRY_RUN_LIMIT {
            return Ok((DRY_RUN_LIMIT as usize, true));
        }
    }
}

/// Ein verstaendlicher Name, wenn der Nutzer keinen eingibt.
///
/// `rule_182991_abc` sagt niemandem etwas. `Rechnung nach Dokumente` schon.
pub fn default_name(rule: &Rule) -> String {
    let ziel = ordner_name(&rule.target);
    let kern = rule.conditions.iter().find_map(|c| match c.field.as_str() {
        "name" | "path" => als_muster(&c.value).first().cloned(),
        "ext" => als_texte(&c.value).first().map(|e| format!(".{e}")),
        "category" => als_texte(&c.value).first().cloned(),
        _ => None,
    });
    match kern {
        Some(k) if !ziel.is_empty() => format!("{k} nach {ziel}"),
        Some(k) => k,
        None if !ziel.is_empty() => format!("Regel nach {ziel}"),
        None => "Neue Regel".into(),
    }
}

/// Wie stark der tatsaechliche Umfang vom bestaetigten abweichen darf.
///
/// Zwischen Vorschau und Bestaetigung koennen Dateien dazukommen (ein Download
/// laeuft) oder wegfallen. Kleine Abweichungen sind normal. Eine grosse
/// bedeutet, dass der Nutzer etwas anderes bestaetigt hat, als jetzt passieren
/// wuerde - dann wird abgebrochen.
const ABWEICHUNG_ABSOLUT: usize = 10;
const ABWEICHUNG_ANTEIL: f64 = 0.25;

/// Fuehrt eine Regel aus. Wird nur nach ausdruecklicher Bestaetigung aufgerufen.
///
/// `bestaetigt_fuer` ist die Anzahl aus der Vorschau, die der Nutzer gesehen
/// hat. Weicht der jetzt ermittelte Umfang deutlich davon ab, passiert nichts.
pub fn apply(
    conn: &Connection,
    rule_id: i64,
    root: Option<&str>,
    policy: CollisionPolicy,
    batch_id: &str,
    bestaetigt_fuer: Option<usize>,
) -> Result<Vec<OpResult>> {
    // Der Dry Run wird unmittelbar vor der Ausfuehrung wiederholt. Der zuvor
    // angezeigte Stand ist zu diesem Zeitpunkt schon Vergangenheit.
    let vorschau = dry_run(conn, rule_id, root)?;

    if let Some(erwartet) = bestaetigt_fuer {
        let jetzt = vorschau.count;
        let abweichung = jetzt.abs_diff(erwartet);
        let grenze = ABWEICHUNG_ABSOLUT.max((erwartet as f64 * ABWEICHUNG_ANTEIL) as usize);
        if abweichung > grenze {
            return Err(AppError::Changed(format!(
                "Die Regel wuerde jetzt {jetzt} statt der bestaetigten {erwartet} Dateien \
                 betreffen. Zur Sicherheit wurde nichts ausgefuehrt - bitte die Vorschau \
                 erneut ansehen."
            )));
        }
    }

    let ergebnisse: Vec<OpResult> = vorschau
        .files
        .iter()
        .map(|f| {
            // `move_file` liest den Zustand der Datei selbst frisch von der
            // Platte und prueft ihn unmittelbar vor dem Verschieben erneut.
            // Der Indexstand dient hier nur der Auswahl, nie als Beweis.
            fileops::move_file(
                conn,
                Path::new(&f.path),
                Path::new(&vorschau.target),
                policy,
                Some(batch_id),
            )
        })
        .collect();
    let erfolge = ergebnisse.iter().filter(|r| r.status == fileops::OpStatus::Done).count();
    conn.execute(
        "UPDATE rules SET hits = hits + ?1 WHERE id = ?2",
        rusqlite::params![erfolge as i64, rule_id],
    )?;
    Ok(ergebnisse)
}

// ------------------------------------------------------------- Vorschlaege

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Suggestion {
    pub ext: String,
    pub category: String,
    pub target: String,
    pub count: i64,
    pub last_ts: i64,
}

/// Vorschlaege aus der Verschiebestatistik. Ein Vorschlag verschiebt nichts.
pub fn suggestions(conn: &Connection, threshold: i64) -> Result<Vec<Suggestion>> {
    let mut stmt = conn.prepare(
        "SELECT ext, category, target, count, last_ts FROM move_stats
         WHERE count >= ?1 AND dismissed = 0
         ORDER BY count DESC, last_ts DESC LIMIT 20",
    )?;
    let mut vorschlaege = stmt
        .query_map([threshold.max(1)], |r| {
            let ext: String = r.get(0)?;
            let category: String = r.get(1)?;
            let target: String = r.get(2)?;
            let count: i64 = r.get(3)?;
            Ok(Suggestion {
                ext,
                category,
                target,
                count,
                last_ts: r.get(4)?,
            })
        })?
        .collect::<std::result::Result<Vec<Suggestion>, _>>()?;

    // Bereits als Regel vorhandene Kombinationen nicht erneut vorschlagen.
    let regeln = list(conn)?;
    vorschlaege.retain(|v| {
        !regeln.iter().any(|r| {
            crate::paths::path_key(&r.target) == crate::paths::path_key(&v.target)
                && r.conditions.iter().any(|c| {
                    c.field == "ext" && als_texte(&c.value).contains(&v.ext.to_lowercase())
                })
        })
    });
    Ok(vorschlaege)
}

/// Merkt sich, dass ein Vorschlag abgelehnt wurde - er kommt nicht wieder.
pub fn dismiss(conn: &Connection, ext: &str, category: &str, target: &str) -> Result<()> {
    conn.execute(
        "UPDATE move_stats SET dismissed = 1
         WHERE ext = ?1 AND category = ?2 AND target = ?3",
        rusqlite::params![ext, category, target],
    )?;
    Ok(())
}

/// Macht aus einem Vorschlag eine Regel - nur auf ausdrueckliche Zustimmung.
pub fn rule_from_suggestion(s: &Suggestion) -> Rule {
    let (name, condition) = if s.ext.is_empty() {
        (
            format!("{} nach {}", s.category, ordner_name(&s.target)),
            Condition {
                field: "category".into(),
                op: "in".into(),
                value: serde_json::json!([s.category]),
            },
        )
    } else {
        (
            format!(".{} nach {}", s.ext, ordner_name(&s.target)),
            Condition {
                field: "ext".into(),
                op: "in".into(),
                value: serde_json::json!([s.ext]),
            },
        )
    };
    Rule {
        id: None,
        name,
        enabled: true,
        priority: 100,
        conditions: vec![condition],
        match_mode: standard_verknuepfung(),
        target: s.target.clone(),
        source: "suggested".into(),
        created_at: 0,
        hits: 0,
    }
}

fn ordner_name(pfad: &str) -> String {
    pfad.trim_end_matches('\\').rsplit('\\').next().unwrap_or(pfad).to_string()
}

/// Zielvorschlag fuer eine einzelne Datei in der Triage:
/// erst passende Regeln, dann die Verschiebestatistik.
pub fn suggest_folder(
    conn: &Connection,
    file: &FileEntry,
    threshold: i64,
) -> Result<Option<(String, String, Option<String>, Option<i64>)>> {
    let now = db::now_ts();
    if let Some(rule) = list(conn)?.into_iter().find(|r| r.matches(file, now)) {
        return Ok(Some((rule.target, "rule".into(), Some(rule.name), None)));
    }

    let mut stmt = conn.prepare(
        "SELECT target, count FROM move_stats
         WHERE dismissed = 0 AND count >= ?1 AND ((ext = ?2 AND ext <> '') OR category = ?3)
         ORDER BY (ext = ?2) DESC, count DESC LIMIT 1",
    )?;
    let mut rows = stmt.query(rusqlite::params![threshold.max(1), file.ext, file.category])?;
    match rows.next()? {
        Some(r) => {
            let target: String = r.get(0)?;
            let count: i64 = r.get(1)?;
            Ok(Some((target, "history".into(), None, Some(count))))
        }
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::testutil::TempDir;

    fn datei(name: &str, ext: &str, category: &str, size: i64, alter_tage: i64) -> FileEntry {
        FileEntry {
            id: 1,
            path: format!("C:\\Downloads\\{name}"),
            parent: "C:\\Downloads".into(),
            name: name.into(),
            ext: ext.into(),
            size,
            created: 0,
            modified: db::now_ts() - alter_tage * 86_400,
            category: category.into(),
            state: "pending".into(),
        }
    }

    fn regel(conditions: Vec<Condition>) -> Rule {
        Rule {
            id: None,
            name: "Test".into(),
            enabled: true,
            priority: 100,
            conditions,
            match_mode: "all".into(),
            target: "C:\\Ziel".into(),
            source: "user".into(),
            created_at: 0,
            hits: 0,
        }
    }

    #[test]
    fn endung_und_namensteil_muessen_beide_passen() {
        let r = regel(vec![
            Condition { field: "ext".into(), op: "in".into(), value: serde_json::json!(["pdf"]) },
            Condition {
                field: "name".into(),
                op: "contains".into(),
                value: serde_json::json!(["rechnung", "invoice"]),
            },
        ]);
        let now = db::now_ts();
        assert!(r.matches(&datei("Rechnung_2026.pdf", "pdf", "pdf", 100, 1), now));
        assert!(r.matches(&datei("INVOICE-9.pdf", "pdf", "pdf", 100, 1), now));
        assert!(!r.matches(&datei("Urlaub.pdf", "pdf", "pdf", 100, 1), now));
        assert!(!r.matches(&datei("rechnung.docx", "docx", "document", 100, 1), now));
    }

    #[test]
    fn alter_und_groesse_vergleichen_richtig() {
        let now = db::now_ts();
        let alt = regel(vec![Condition {
            field: "age_days".into(),
            op: "gt".into(),
            value: serde_json::json!(30),
        }]);
        assert!(alt.matches(&datei("a.exe", "exe", "installer", 1, 60), now));
        assert!(!alt.matches(&datei("a.exe", "exe", "installer", 1, 5), now));

        let gross = regel(vec![Condition {
            field: "size".into(),
            op: "gt".into(),
            value: serde_json::json!(1_000_000),
        }]);
        assert!(gross.matches(&datei("f.iso", "iso", "disk_image", 2_000_000, 1), now));
        assert!(!gross.matches(&datei("f.iso", "iso", "disk_image", 500, 1), now));
    }

    #[test]
    fn abgeschaltete_regel_trifft_nie() {
        let mut r = regel(vec![Condition {
            field: "ext".into(),
            op: "in".into(),
            value: serde_json::json!(["pdf"]),
        }]);
        r.enabled = false;
        assert!(!r.matches(&datei("x.pdf", "pdf", "pdf", 1, 1), db::now_ts()));
    }

    #[test]
    fn regel_ohne_bedingung_wird_abgelehnt() {
        assert!(regel(vec![]).validate().is_err());
    }

    #[test]
    fn relatives_ziel_wird_abgelehnt() {
        let mut r = regel(vec![Condition {
            field: "ext".into(),
            op: "in".into(),
            value: serde_json::json!(["pdf"]),
        }]);
        r.target = "..\\woanders".into();
        assert!(r.validate().is_err());
    }

    #[test]
    fn speichern_lesen_aendern_loeschen() {
        let dir = TempDir::new("rules");
        let db = Db::open(dir.path().join("i.db")).unwrap();
        let conn = db.conn().unwrap();

        let id = save(
            &conn,
            &regel(vec![Condition {
                field: "ext".into(),
                op: "in".into(),
                value: serde_json::json!(["pdf"]),
            }]),
        )
        .unwrap();
        assert_eq!(list(&conn).unwrap().len(), 1);

        let mut r = by_id(&conn, id).unwrap();
        r.name = "Umbenannt".into();
        r.enabled = false;
        save(&conn, &r).unwrap();
        let geladen = by_id(&conn, id).unwrap();
        assert_eq!(geladen.name, "Umbenannt");
        assert!(!geladen.enabled);

        delete(&conn, id).unwrap();
        assert!(list(&conn).unwrap().is_empty());
    }

    #[test]
    fn vorschlag_erst_ab_schwelle_und_wird_zur_regel() {
        let dir = TempDir::new("sugg");
        let db = Db::open(dir.path().join("i.db")).unwrap();
        let conn = db.conn().unwrap();
        let ziel = dir.dir("Rechnungen");

        for _ in 0..2 {
            fileops::record_move_stat(&conn, "beleg.pdf", &ziel).unwrap();
        }
        assert!(suggestions(&conn, 3).unwrap().is_empty(), "unter der Schwelle kein Vorschlag");

        fileops::record_move_stat(&conn, "beleg.pdf", &ziel).unwrap();
        let v = suggestions(&conn, 3).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].count, 3);

        let regel = rule_from_suggestion(&v[0]);
        assert_eq!(regel.source, "suggested");
        save(&conn, &regel).unwrap();
        // Danach nicht noch einmal vorschlagen.
        assert!(suggestions(&conn, 3).unwrap().is_empty());
    }

    /// Angriff: Zwischen Vorschau und Bestaetigung wachsen die Treffer stark
    /// an - etwa weil ein Download-Ordner sich gerade fuellt. Der Nutzer haette
    /// dann etwas anderes bestaetigt, als tatsaechlich passieren wuerde.
    #[test]
    fn starke_abweichung_von_der_vorschau_stoppt_die_ausfuehrung() {
        use crate::config::ScanSettings;
        use crate::scan::{ScanOptions, scan};
        use std::sync::atomic::AtomicBool;

        let dir = TempDir::new("abweichung");
        dir.file("a.pdf", b"x");
        let db = Db::open(dir.path().join("idx").join("i.db")).unwrap();
        let scanne = || {
            scan(
                &db,
                &ScanOptions {
                    root: dir.path().to_path_buf(),
                    settings: ScanSettings::default(),
                },
                &AtomicBool::new(false),
                &mut |_| {},
            )
            .unwrap();
        };
        scanne();

        let conn = db.conn().unwrap();
        let ziel = dir.dir("ziel");
        let id = save(
            &conn,
            &Rule {
                target: ziel.display().to_string(),
                ..regel(vec![Condition {
                    field: "ext".into(),
                    op: "in".into(),
                    value: serde_json::json!(["pdf"]),
                }])
            },
        )
        .unwrap();

        let vorschau = dry_run(&conn, id, None).unwrap();
        assert_eq!(vorschau.count, 1);

        // Zwischen Vorschau und Bestaetigung landen 60 weitere PDFs im Ordner.
        for i in 0..60 {
            dir.file(&format!("neu{i}.pdf"), b"x");
        }
        scanne();

        let fehler = apply(&conn, id, None, CollisionPolicy::Rename, "b1", Some(vorschau.count))
            .unwrap_err();
        assert_eq!(fehler.kind(), "changed", "{fehler}");
        assert!(dir.path().join("a.pdf").exists(), "es darf nichts verschoben worden sein");
        assert_eq!(
            std::fs::read_dir(&ziel).unwrap().count(),
            0,
            "am Ziel darf nichts angekommen sein"
        );
    }

    #[test]
    fn kleine_abweichung_ist_kein_grund_zum_abbruch() {
        use crate::config::ScanSettings;
        use crate::scan::{ScanOptions, scan};
        use std::sync::atomic::AtomicBool;

        let dir = TempDir::new("abweichung-klein");
        for i in 0..3 {
            dir.file(&format!("r{i}.pdf"), b"x");
        }
        let db = Db::open(dir.path().join("idx").join("i.db")).unwrap();
        scan(
            &db,
            &ScanOptions { root: dir.path().to_path_buf(), settings: ScanSettings::default() },
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .unwrap();

        let conn = db.conn().unwrap();
        let ziel = dir.dir("ziel");
        let id = save(
            &conn,
            &Rule {
                target: ziel.display().to_string(),
                ..regel(vec![Condition {
                    field: "ext".into(),
                    op: "in".into(),
                    value: serde_json::json!(["pdf"]),
                }])
            },
        )
        .unwrap();

        // Der Nutzer hatte 4 gesehen, jetzt sind es 3 - das ist im Rahmen.
        let ergebnisse = apply(&conn, id, None, CollisionPolicy::Rename, "b1", Some(4)).unwrap();
        assert_eq!(ergebnisse.len(), 3);
    }

    /// Der Dry Run muss ueber alle Seiten zaehlen. Eine Zahl, die nur die
    /// erste Seite umfasst, waere schlimmer als keine: Der Nutzer bestaetigt
    /// "183 Dateien" und bekommt Tausende.
    #[test]
    fn dry_run_zaehlt_ueber_seitengrenzen_hinweg() {
        use crate::config::ScanSettings;
        use crate::scan::{ScanOptions, scan};
        use std::sync::atomic::AtomicBool;

        let dir = TempDir::new("dry-seiten");
        let feld = dir.dir("feld");
        // Deutlich mehr als eine Seite (2.000) der internen Abfrage.
        for i in 0..2_500 {
            std::fs::write(feld.join(format!("r{i}.pdf")), b"x").unwrap();
        }
        for i in 0..100 {
            std::fs::write(feld.join(format!("x{i}.txt")), b"x").unwrap();
        }

        let db = Db::open(dir.path().join("idx").join("i.db")).unwrap();
        scan(
            &db,
            &ScanOptions { root: feld.clone(), settings: ScanSettings::default() },
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .unwrap();

        let conn = db.conn().unwrap();
        let id = save(
            &conn,
            &Rule {
                target: dir.dir("ziel").display().to_string(),
                ..regel(vec![Condition {
                    field: "ext".into(),
                    op: "in".into(),
                    value: serde_json::json!(["pdf"]),
                }])
            },
        )
        .unwrap();

        let lauf = dry_run(&conn, id, None).unwrap();
        assert_eq!(lauf.count, 2_500, "alle Treffer müssen gezaehlt werden");
        assert!(!lauf.truncated);
        assert_eq!(lauf.files.len(), 2_500);
    }

    #[test]
    fn abgelehnter_vorschlag_kommt_nicht_wieder() {
        let dir = TempDir::new("dismiss");
        let db = Db::open(dir.path().join("i.db")).unwrap();
        let conn = db.conn().unwrap();
        let ziel = dir.dir("Archiv");
        for _ in 0..3 {
            fileops::record_move_stat(&conn, "a.zip", &ziel).unwrap();
        }
        let v = suggestions(&conn, 3).unwrap();
        assert_eq!(v.len(), 1);
        dismiss(&conn, &v[0].ext, &v[0].category, &v[0].target).unwrap();
        assert!(suggestions(&conn, 3).unwrap().is_empty());
    }

    #[test]
    fn dry_run_veraendert_nichts() {
        use crate::config::ScanSettings;
        use crate::scan::{ScanOptions, scan};
        use std::sync::atomic::AtomicBool;

        let dir = TempDir::new("dry");
        dir.file("rechnung1.pdf", b"x");
        dir.file("rechnung2.pdf", b"x");
        dir.file("urlaub.jpg", b"x");
        let db = Db::open(dir.path().join(".i.db")).unwrap();
        scan(
            &db,
            &ScanOptions { root: dir.path().to_path_buf(), settings: ScanSettings::default() },
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .unwrap();

        let conn = db.conn().unwrap();
        let ziel = dir.dir("Rechnungen");
        let id = save(
            &conn,
            &Rule {
                target: ziel.display().to_string(),
                ..regel(vec![
                    Condition { field: "ext".into(), op: "in".into(), value: serde_json::json!(["pdf"]) },
                    Condition {
                        field: "name".into(),
                        op: "contains".into(),
                        value: serde_json::json!(["rechnung"]),
                    },
                ])
            },
        )
        .unwrap();

        let vorschau = dry_run(&conn, id, None).unwrap();
        assert_eq!(vorschau.count, 2);
        assert!(!vorschau.truncated);
        assert!(dir.path().join("rechnung1.pdf").exists(), "Dry Run darf nichts bewegen");

        let ergebnisse = apply(&conn, id, None, CollisionPolicy::Rename, "b1", Some(2)).unwrap();
        assert_eq!(ergebnisse.iter().filter(|r| r.status == fileops::OpStatus::Done).count(), 2);
        assert!(ziel.join("rechnung1.pdf").exists());
        assert!(dir.path().join("urlaub.jpg").exists(), "andere Dateien bleiben liegen");
    }

    // -----------------------------------------------------------------
    // Dateinamensbedingungen
    // -----------------------------------------------------------------

    fn namensregel(op: &str, werte: serde_json::Value) -> Rule {
        regel(vec![Condition { field: "name".into(), op: op.into(), value: werte }])
    }

    #[test]
    fn dateiname_enthaelt_findet_ueberall_im_namen() {
        let r = namensregel("contains", serde_json::json!(["rechnung"]));
        let now = db::now_ts();
        for treffer in ["Rechnung_August.pdf", "stromrechnung.pdf", "meine_rechnung_2026.docx"] {
            assert!(r.matches(&datei(treffer, "pdf", "pdf", 1, 1), now), "{treffer}");
        }
        assert!(!r.matches(&datei("Urlaub.jpg", "jpg", "image", 1, 1), now));
    }

    #[test]
    fn dateiname_beginnt_mit_ist_am_anfang_verankert() {
        let r = namensregel("starts_with", serde_json::json!(["invoice_"]));
        let now = db::now_ts();
        assert!(r.matches(&datei("invoice_001.pdf", "pdf", "pdf", 1, 1), now));
        assert!(r.matches(&datei("invoice_amazon.pdf", "pdf", "pdf", 1, 1), now));
        assert!(
            !r.matches(&datei("old_invoice_001.pdf", "pdf", "pdf", 1, 1), now),
            "was vorne etwas anderes stehen hat, faengt nicht damit an"
        );
    }

    /// `ends_with` meint beim Namen den Teil **vor** der Endung. Wer wirklich
    /// die Endung meint, wird trotzdem bedient - sonst waere eine eingetippte
    /// Endung wirkungslos, ohne dass jemand merkt, warum.
    #[test]
    fn dateiname_endet_mit_meint_den_teil_vor_der_endung() {
        let r = namensregel("ends_with", serde_json::json!(["_backup"]));
        let now = db::now_ts();
        assert!(r.matches(&datei("projekt_backup.zip", "zip", "archive", 1, 1), now));
        assert!(r.matches(&datei("db_backup.sql", "sql", "code", 1, 1), now));
        assert!(!r.matches(&datei("backup_projekt.zip", "zip", "archive", 1, 1), now));

        let endung = namensregel("ends_with", serde_json::json!([".zip"]));
        assert!(
            endung.matches(&datei("projekt.zip", "zip", "archive", 1, 1), now),
            "eine eingetippte Endung darf nicht ins Leere greifen"
        );
    }

    #[test]
    fn dateiname_ist_genau_vergleicht_den_ganzen_namen() {
        let r = namensregel("eq", serde_json::json!(["report_final.pdf"]));
        let now = db::now_ts();
        assert!(r.matches(&datei("report_final.pdf", "pdf", "pdf", 1, 1), now));
        assert!(r.matches(&datei("Report_Final.PDF", "pdf", "pdf", 1, 1), now));
        assert!(!r.matches(&datei("report_final_2.pdf", "pdf", "pdf", 1, 1), now));
        assert!(!r.matches(&datei("mein_report_final.pdf", "pdf", "pdf", 1, 1), now));
    }

    /// Windows-Dateinamen unterscheiden Gross- und Kleinschreibung nicht.
    /// Eine Regel, die das anders sieht, waere eine boese Ueberraschung.
    #[test]
    fn gross_und_kleinschreibung_ist_egal() {
        let r = namensregel("contains", serde_json::json!(["rechnung"]));
        let now = db::now_ts();
        for name in ["Rechnung.pdf", "RECHNUNG.pdf", "ReChNuNg.pdf", "rechnung.pdf"] {
            assert!(r.matches(&datei(name, "pdf", "pdf", 1, 1), now), "{name}");
        }
        // Und andersherum: auch das Muster darf gross geschrieben sein.
        let gross = namensregel("contains", serde_json::json!(["RECHNUNG"]));
        assert!(gross.matches(&datei("rechnung.pdf", "pdf", "pdf", 1, 1), now));
    }

    /// Umlaute, Akzente, Koreanisch, Japanisch, Emoji. Regeln lesen nur - sie
    /// duerfen keinen Namen veraendern und an keinem Zeichen scheitern.
    #[test]
    fn namensregeln_kommen_mit_unicode_zurecht() {
        let now = db::now_ts();
        let faelle: [(&str, &str, bool); 6] = [
            ("übersicht", "Rechnung_Übersicht.pdf", true),
            ("café", "CAFÉ_Rechnung.pdf", true),
            ("영수증", "영수증_2026.pdf", true),
            ("請求書", "請求書_08.pdf", true),
            ("💶", "Geld_💶.pdf", true),
            ("übersicht", "Uebersicht.pdf", false),
        ];
        for (muster, name, erwartet) in faelle {
            let r = namensregel("contains", serde_json::json!([muster]));
            assert_eq!(
                r.matches(&datei(name, "pdf", "pdf", 1, 1), now),
                erwartet,
                "Muster {muster} auf {name}"
            );
        }
    }

    /// Ein fuehrender Punkt gehoert beim Namen zum Muster. Bei der Endung
    /// nicht - dort meinen ".pdf" und "pdf" dasselbe.
    #[test]
    fn fuehrender_punkt_bleibt_im_namensmuster_stehen() {
        let now = db::now_ts();
        let r = namensregel("contains", serde_json::json!([".backup"]));
        assert!(r.matches(&datei("daten.backup.zip", "zip", "archive", 1, 1), now));
        assert!(
            !r.matches(&datei("daten_backup.zip", "zip", "archive", 1, 1), now),
            "ohne Punkt ist es ein anderer Name"
        );

        let e = regel(vec![Condition {
            field: "ext".into(),
            op: "in".into(),
            value: serde_json::json!([".pdf"]),
        }]);
        assert!(e.matches(&datei("x.pdf", "pdf", "pdf", 1, 1), now));
    }

    #[test]
    fn mehrere_bedingungen_muessen_standardmaessig_alle_zutreffen() {
        let r = regel(vec![
            Condition {
                field: "name".into(),
                op: "contains".into(),
                value: serde_json::json!(["rechnung"]),
            },
            Condition { field: "ext".into(), op: "in".into(), value: serde_json::json!(["pdf"]) },
        ]);
        assert_eq!(r.match_mode, "all", "Voreinstellung ist UND");
        let now = db::now_ts();
        assert!(r.matches(&datei("Rechnung_1.pdf", "pdf", "pdf", 1, 1), now));
        assert!(!r.matches(&datei("Rechnung_1.docx", "docx", "document", 1, 1), now));
        assert!(!r.matches(&datei("Urlaub.pdf", "pdf", "pdf", 1, 1), now));
    }

    #[test]
    fn bei_oder_reicht_eine_zutreffende_bedingung() {
        let mut r = regel(vec![
            Condition {
                field: "name".into(),
                op: "contains".into(),
                value: serde_json::json!(["rechnung"]),
            },
            Condition {
                field: "name".into(),
                op: "contains".into(),
                value: serde_json::json!(["invoice"]),
            },
        ]);
        r.match_mode = "any".into();
        let now = db::now_ts();
        assert!(r.matches(&datei("Rechnung_1.pdf", "pdf", "pdf", 1, 1), now));
        assert!(r.matches(&datei("invoice_2.pdf", "pdf", "pdf", 1, 1), now));
        assert!(!r.matches(&datei("Urlaub.pdf", "pdf", "pdf", 1, 1), now));
    }

    #[test]
    fn unbekannte_verknuepfung_wird_abgelehnt() {
        let mut r = namensregel("contains", serde_json::json!(["a"]));
        r.match_mode = "vielleicht".into();
        assert!(r.validate().is_err());
    }

    /// Eine Bedingung ohne Text wuerde je nach Vergleich auf alles passen -
    /// und damit einen ganzen Ordner verschieben, den niemand gemeint hat.
    #[test]
    fn leere_bedingung_wird_abgelehnt() {
        assert!(namensregel("contains", serde_json::json!([])).validate().is_err());
        assert!(namensregel("contains", serde_json::json!([""])).validate().is_err());
        assert!(namensregel("contains", serde_json::json!(["   "])).validate().is_err());
    }

    #[test]
    fn vergleich_und_feld_muessen_zusammenpassen() {
        // "groesser als" auf einem Dateinamen ergibt keinen Sinn.
        let r = namensregel("gt", serde_json::json!(["a"]));
        assert!(r.validate().is_err());
    }

    #[test]
    fn ohne_namen_entsteht_ein_verstaendlicher_name() {
        let dir = TempDir::new("autoname");
        let db = Db::open(dir.path().join("i.db")).unwrap();
        let conn = db.conn().unwrap();

        let mut r = namensregel("contains", serde_json::json!(["rechnung"]));
        r.name = String::new();
        r.target = "C:\\Users\\Test\\Documents\\Rechnungen".into();
        let id = save(&conn, &r).unwrap();
        assert_eq!(by_id(&conn, id).unwrap().name, "rechnung nach Rechnungen");

        let mut e = regel(vec![Condition {
            field: "ext".into(),
            op: "in".into(),
            value: serde_json::json!(["png"]),
        }]);
        e.name = "   ".into();
        e.target = "C:\\Users\\Test\\Pictures".into();
        let id = save(&conn, &e).unwrap();
        assert_eq!(by_id(&conn, id).unwrap().name, ".png nach Pictures");
    }

    #[test]
    fn verknuepfung_ueberlebt_speichern_und_laden() {
        let dir = TempDir::new("modus");
        let db = Db::open(dir.path().join("i.db")).unwrap();
        let conn = db.conn().unwrap();
        let mut r = namensregel("contains", serde_json::json!(["a"]));
        r.match_mode = "any".into();
        let id = save(&conn, &r).unwrap();
        assert_eq!(by_id(&conn, id).unwrap().match_mode, "any");

        let mut zurueck = by_id(&conn, id).unwrap();
        zurueck.match_mode = "all".into();
        save(&conn, &zurueck).unwrap();
        assert_eq!(by_id(&conn, id).unwrap().match_mode, "all");
    }

    /// Der Kernfall aus der Anforderung, vollstaendig durchgespielt.
    #[test]
    fn rechnung_nach_dokumente_rechnungen() {
        use crate::config::ScanSettings;
        use crate::scan::{ScanOptions, scan};
        use std::sync::atomic::AtomicBool;

        let dir = TempDir::new("rechnungen");
        let quelle = dir.dir("Downloads");
        for n in [
            "Rechnung_Amazon.pdf",
            "stromrechnung.pdf",
            "meine_rechnung_2026.pdf",
            "Urlaub.jpg",
            "Rechnung_Notiz.txt",
        ] {
            std::fs::write(quelle.join(n), b"x").unwrap();
        }
        let ziel = dir.dir("Documents\\Rechnungen");

        let db = Db::open(dir.path().join("idx").join("i.db")).unwrap();
        scan(
            &db,
            &ScanOptions { root: quelle.clone(), settings: ScanSettings::default() },
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .unwrap();
        let conn = db.conn().unwrap();

        let id = save(
            &conn,
            &Rule {
                name: String::new(),
                target: ziel.display().to_string(),
                ..regel(vec![
                    Condition {
                        field: "name".into(),
                        op: "contains".into(),
                        value: serde_json::json!(["rechnung"]),
                    },
                    Condition {
                        field: "ext".into(),
                        op: "in".into(),
                        value: serde_json::json!(["pdf"]),
                    },
                ])
            },
        )
        .unwrap();

        let lauf = dry_run(&conn, id, None).unwrap();
        assert_eq!(lauf.count, 3, "die drei PDFs, nicht das Bild und nicht die Notiz");
        assert_eq!(lauf.conflicts, 0);
        assert_eq!(lauf.safe, 3);
        assert!(quelle.join("Rechnung_Amazon.pdf").exists(), "die Vorschau bewegt nichts");

        let erg = apply(&conn, id, None, CollisionPolicy::Rename, "b1", Some(3)).unwrap();
        assert_eq!(erg.iter().filter(|r| r.status == fileops::OpStatus::Done).count(), 3);
        assert!(ziel.join("Rechnung_Amazon.pdf").exists());
        assert!(quelle.join("Urlaub.jpg").exists());
        assert!(quelle.join("Rechnung_Notiz.txt").exists());
    }

    /// Namenskonflikte muessen **vor** der Ausfuehrung sichtbar sein. Sie sind
    /// nicht gefaehrlich - die Datei bekommt eine Nummer -, aber der Nutzer
    /// soll nicht ueberrascht werden.
    #[test]
    fn die_vorschau_zaehlt_namenskonflikte() {
        use crate::config::ScanSettings;
        use crate::scan::{ScanOptions, scan};
        use std::sync::atomic::AtomicBool;

        let dir = TempDir::new("konflikte");
        let quelle = dir.dir("quelle");
        for i in 0..4 {
            std::fs::write(quelle.join(format!("beleg{i}.pdf")), b"neu").unwrap();
        }
        let ziel = dir.dir("ziel");
        // Zwei Namen gibt es dort schon.
        std::fs::write(ziel.join("beleg0.pdf"), b"alt").unwrap();
        std::fs::write(ziel.join("beleg1.pdf"), b"alt").unwrap();

        let db = Db::open(dir.path().join("idx").join("i.db")).unwrap();
        scan(
            &db,
            &ScanOptions { root: quelle.clone(), settings: ScanSettings::default() },
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .unwrap();
        let conn = db.conn().unwrap();
        let id = save(
            &conn,
            &Rule {
                target: ziel.display().to_string(),
                ..regel(vec![Condition {
                    field: "ext".into(),
                    op: "in".into(),
                    value: serde_json::json!(["pdf"]),
                }])
            },
        )
        .unwrap();

        let lauf = dry_run(&conn, id, None).unwrap();
        assert_eq!(lauf.count, 4);
        assert_eq!(lauf.conflicts, 2);
        assert_eq!(lauf.safe, 2);
        assert_eq!(lauf.conflicts + lauf.safe, lauf.count);
        assert_eq!(std::fs::read(ziel.join("beleg0.pdf")).unwrap(), b"alt");
    }

    /// Der Fehler, den es hier schon einmal gab: eine Zahl, die nur die erste
    /// Seite umfasst. Bei 3.300 Treffern muss die Vorschau 3.300 sagen - auch
    /// die Gesamtgroesse und die Konfliktzahl.
    #[test]
    fn vorschauzahlen_umfassen_alle_seiten() {
        use crate::config::ScanSettings;
        use crate::scan::{ScanOptions, scan};
        use std::sync::atomic::AtomicBool;

        let dir = TempDir::new("seiten-3300");
        let quelle = dir.dir("quelle");
        for i in 0..3_300 {
            std::fs::write(quelle.join(format!("r{i}.pdf")), b"abcde").unwrap();
        }
        let db = Db::open(dir.path().join("idx").join("i.db")).unwrap();
        scan(
            &db,
            &ScanOptions { root: quelle.clone(), settings: ScanSettings::default() },
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .unwrap();

        let conn = db.conn().unwrap();
        let regel_id = save(
            &conn,
            &Rule {
                target: dir.dir("ziel").display().to_string(),
                ..regel(vec![Condition {
                    field: "ext".into(),
                    op: "in".into(),
                    value: serde_json::json!(["pdf"]),
                }])
            },
        )
        .unwrap();

        let lauf = dry_run(&conn, regel_id, None).unwrap();
        assert_eq!(lauf.count, 3_300);
        assert_eq!(lauf.files.len(), 3_300);
        assert_eq!(lauf.total_size, 3_300 * 5);
        assert_eq!(lauf.safe, 3_300);
        assert!(!lauf.truncated);

        // Und die schnelle Zaehlung fuer die Regelliste muss dieselbe Zahl
        // liefern wie die Vorschau. Zwei Zahlen, die sich widersprechen,
        // waeren schlimmer als eine fehlende.
        let regel = by_id(&conn, regel_id).unwrap();
        let (schnell, abgeschnitten) = match_count(&conn, &regel, None).unwrap();
        assert_eq!(schnell, 3_300);
        assert!(!abgeschnitten);
    }

    /// Die zentrale Zusage dieses Releases: Eine gespeicherte Regel bewegt von
    /// sich aus nichts. Weder das Speichern noch das Auflisten noch das
    /// Zaehlen der Treffer fasst eine Datei an - erst `apply` tut das, und das
    /// ruft nur die Oberflaeche nach einer Bestaetigung auf.
    #[test]
    fn eine_gespeicherte_regel_verschiebt_von_sich_aus_nichts() {
        use crate::config::ScanSettings;
        use crate::scan::{ScanOptions, scan};
        use std::sync::atomic::AtomicBool;

        let dir = TempDir::new("keine-automatik");
        let quelle = dir.dir("quelle");
        for i in 0..5 {
            std::fs::write(quelle.join(format!("rechnung{i}.pdf")), b"x").unwrap();
        }
        let ziel = dir.dir("ziel");
        let db = Db::open(dir.path().join("idx").join("i.db")).unwrap();
        let optionen =
            ScanOptions { root: quelle.clone(), settings: ScanSettings::default() };
        scan(&db, &optionen, &AtomicBool::new(false), &mut |_| {}).unwrap();

        let conn = db.conn().unwrap();
        let id = save(
            &conn,
            &Rule {
                target: ziel.display().to_string(),
                ..regel(vec![Condition {
                    field: "name".into(),
                    op: "contains".into(),
                    value: serde_json::json!(["rechnung"]),
                }])
            },
        )
        .unwrap();

        // Alles, was ohne ausdrueckliche Bestaetigung passieren kann.
        let regel = by_id(&conn, id).unwrap();
        let _ = list(&conn).unwrap();
        let _ = match_count(&conn, &regel, None).unwrap();
        let _ = dry_run(&conn, id, None).unwrap();
        let _ = suggestions(&conn, 3).unwrap();
        let _ = crate::patterns::suggestions(&conn, 3).unwrap();
        // Und ein erneuter Scan, als liefe die Ueberwachung.
        scan(&db, &optionen, &AtomicBool::new(false), &mut |_| {}).unwrap();

        assert_eq!(
            std::fs::read_dir(&ziel).unwrap().count(),
            0,
            "ohne Bestaetigung darf im Ziel nichts liegen"
        );
        assert_eq!(std::fs::read_dir(&quelle).unwrap().count(), 5);
    }

    /// Eine abgeschaltete Regel wird auch im Stapel nicht angewendet.
    #[test]
    fn abgeschaltete_regel_trifft_auch_in_der_vorschau_nichts() {
        use crate::config::ScanSettings;
        use crate::scan::{ScanOptions, scan};
        use std::sync::atomic::AtomicBool;

        let dir = TempDir::new("aus");
        dir.file("rechnung.pdf", b"x");
        let db = Db::open(dir.path().join("idx").join("i.db")).unwrap();
        scan(
            &db,
            &ScanOptions { root: dir.path().to_path_buf(), settings: ScanSettings::default() },
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .unwrap();
        let conn = db.conn().unwrap();
        let id = save(
            &conn,
            &Rule {
                enabled: false,
                target: dir.dir("ziel").display().to_string(),
                ..regel(vec![Condition {
                    field: "ext".into(),
                    op: "in".into(),
                    value: serde_json::json!(["pdf"]),
                }])
            },
        )
        .unwrap();

        assert_eq!(dry_run(&conn, id, None).unwrap().count, 0);
        let erg = apply(&conn, id, None, CollisionPolicy::Rename, "b1", Some(0)).unwrap();
        assert!(erg.is_empty());
        assert!(dir.path().join("rechnung.pdf").exists());
    }
}
