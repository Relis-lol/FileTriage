//! Duplikaterkennung in drei Stufen.
//!
//! 1. **Groesse** - nur Dateien gleicher Groesse koennen identisch sein. Das ist
//!    reine Datenbankarbeit und kostet nichts.
//! 2. **Teil-Hash** - Anfang und Ende der Datei. Trennt in der Praxis fast alle
//!    Zufallstreffer, ohne Gigabytes zu lesen.
//! 3. **Vollstaendiger Hash** - nur noch fuer die verbliebenen Kandidaten.
//!
//! Gleicher Name oder gleiche Groesse allein gilt hier nie als Duplikat.

use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use rayon::prelude::*;
use rusqlite::Connection;
use serde::Serialize;

use crate::db::Db;
use crate::error::{AppError, Result};
use crate::model::FileEntry;
use crate::paths;

/// Wie viele Bytes je Ende fuer den Teil-Hash gelesen werden.
const TEIL_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, Default, Serialize)]
pub struct DupProgress {
    /// 1 = Groessen, 2 = Teil-Hash, 3 = vollstaendiger Hash.
    pub phase: u8,
    pub checked: u64,
    pub total: u64,
    pub groups: u64,
    /// Belegter Platz, der sich durch Entfernen der Kopien sparen liesse.
    pub wasted: i64,
    pub errors: u64,
    pub done: bool,
    pub cancelled: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct DupGroup {
    pub hash: String,
    pub size: i64,
    /// Nach Pfad sortiert; die Oberflaeche schlaegt die erste Datei als
    /// "behalten" vor, entscheidet aber nie selbst.
    pub files: Vec<FileEntry>,
    /// Platz, der tatsaechlich frei wuerde, wenn nur eine Datei bliebe.
    ///
    /// Hardlinks zaehlen hier **nicht** mit: Sie belegen den Speicher nur
    /// einmal, und ihr Loeschen gibt nichts frei.
    pub wasted: i64,
    /// Mindestens zwei Pfade dieser Gruppe zeigen auf dieselben Daten.
    pub hardlinked: bool,
    /// Der Inhalt wurde vollstaendig verglichen - Byte fuer Byte, ueber den
    /// vollstaendigen Hash.
    ///
    /// Das Feld ist immer `true`: Eine Gruppe entsteht nur aus uebereinstimmenden
    /// vollstaendigen Hashes, deren Groesse und Zeitstempel noch zum Index
    /// passen. Es steht trotzdem hier, weil die Oberflaeche eine Zusage macht
    /// ("dieselbe Datei") und diese Zusage an einem Wert haengen soll, den ein
    /// Test pruefen kann - nicht an einer Annahme ueber eine SQL-Abfrage.
    pub verified: bool,
}

struct Kandidat {
    id: i64,
    path: PathBuf,
    size: i64,
    modified: i64,
}

/// Teil-Hash aus Groesse, Anfang und Ende der Datei.
pub fn quick_hash(path: &std::path::Path, size: u64) -> Result<String> {
    let mut datei = std::fs::File::open(paths::extended(path))
        .map_err(|e| AppError::from_io(&e, &format!("Lesen von {}", path.display())))?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(&size.to_le_bytes());

    let mut puffer = vec![0u8; TEIL_BYTES.min(size.max(1)) as usize];
    let gelesen = datei
        .read(&mut puffer)
        .map_err(|e| AppError::from_io(&e, &format!("Lesen von {}", path.display())))?;
    hasher.update(&puffer[..gelesen]);

    if size > TEIL_BYTES * 2 {
        datei
            .seek(SeekFrom::End(-(TEIL_BYTES as i64)))
            .map_err(|e| AppError::from_io(&e, "Positionieren"))?;
        let gelesen = datei
            .read(&mut puffer)
            .map_err(|e| AppError::from_io(&e, "Lesen des Dateiendes"))?;
        hasher.update(&puffer[..gelesen]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

/// Vollstaendiger kryptografischer Hash.
pub fn full_hash(path: &std::path::Path) -> Result<String> {
    let mut datei = std::fs::File::open(paths::extended(path))
        .map_err(|e| AppError::from_io(&e, &format!("Lesen von {}", path.display())))?;
    let mut hasher = blake3::Hasher::new();
    let mut puffer = vec![0u8; 256 * 1024];
    loop {
        let n = datei
            .read(&mut puffer)
            .map_err(|e| AppError::from_io(&e, &format!("Lesen von {}", path.display())))?;
        if n == 0 {
            break;
        }
        hasher.update(&puffer[..n]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

/// Fuehrt die Suche durch und schreibt die Hashes in den Index.
pub fn find(
    db: &Db,
    root: Option<&str>,
    min_size: i64,
    cancel: &AtomicBool,
    on_progress: &mut dyn FnMut(&DupProgress),
) -> Result<DupProgress> {
    let mut conn = db.conn()?;
    let root_id = match root {
        Some(r) => crate::scan::root_id_for(&conn, r)?,
        None => None,
    };
    let min_size = min_size.max(1);

    let mut progress = DupProgress { phase: 1, ..Default::default() };
    on_progress(&progress);

    // --- Stufe 1: gleiche Groesse -----------------------------------------
    let kandidaten = lade_kandidaten(&conn, root_id, min_size, None)?;
    progress.total = kandidaten.len() as u64;
    on_progress(&progress);
    if kandidaten.is_empty() {
        progress.done = true;
        on_progress(&progress);
        return Ok(progress);
    }

    // --- Stufe 2: Teil-Hash ------------------------------------------------
    progress.phase = 2;
    progress.checked = 0;
    let ergebnisse = hashe_parallel(&kandidaten, cancel, false, &mut |n| {
        progress.checked = n;
        on_progress(&progress);
    });
    progress.errors += ergebnisse.iter().filter(|(_, r)| r.is_err()).count() as u64;
    schreibe_hashes(&mut conn, &kandidaten, &ergebnisse, false)?;
    if cancel.load(Ordering::Relaxed) {
        progress.cancelled = true;
        progress.done = true;
        on_progress(&progress);
        return Ok(progress);
    }

    // --- Stufe 3: vollstaendiger Hash, nur fuer echte Kandidaten -----------
    let engere = lade_kandidaten(&conn, root_id, min_size, Some(()))?;
    progress.phase = 3;
    progress.checked = 0;
    progress.total = engere.len() as u64;
    on_progress(&progress);

    if !engere.is_empty() {
        let ergebnisse = hashe_parallel(&engere, cancel, true, &mut |n| {
            progress.checked = n;
            on_progress(&progress);
        });
        progress.errors += ergebnisse.iter().filter(|(_, r)| r.is_err()).count() as u64;
        schreibe_hashes(&mut conn, &engere, &ergebnisse, true)?;
    }

    let gruppen = groups(&conn, root, min_size)?;
    progress.groups = gruppen.len() as u64;
    progress.wasted = gruppen.iter().map(|g| g.wasted).sum();
    progress.cancelled = cancel.load(Ordering::Relaxed);
    progress.done = true;
    on_progress(&progress);
    Ok(progress)
}

/// Kandidaten laden. Ohne `nur_teilhash_gruppen` sind es alle Dateien mit
/// mehrfach vorkommender Groesse, mit ihm nur die, deren Teil-Hash sich
/// mindestens einmal wiederholt.
fn lade_kandidaten(
    conn: &Connection,
    root_id: Option<i64>,
    min_size: i64,
    nur_teilhash_gruppen: Option<()>,
) -> Result<Vec<Kandidat>> {
    let root_bedingung = match root_id {
        Some(_) => "AND root_id = ?2",
        None => "",
    };
    let sql = if nur_teilhash_gruppen.is_some() {
        format!(
            "SELECT id, path, size, modified FROM files
             WHERE size >= ?1 {root_bedingung} AND quick_hash IS NOT NULL
               AND (size, quick_hash) IN (
                   SELECT size, quick_hash FROM files
                   WHERE size >= ?1 {root_bedingung} AND quick_hash IS NOT NULL
                   GROUP BY size, quick_hash HAVING count(*) > 1)"
        )
    } else {
        format!(
            "SELECT id, path, size, modified FROM files
             WHERE size >= ?1 {root_bedingung}
               AND size IN (SELECT size FROM files
                            WHERE size >= ?1 {root_bedingung}
                            GROUP BY size HAVING count(*) > 1)"
        )
    };

    let mut stmt = conn.prepare(&sql)?;
    let abbilden = |row: &rusqlite::Row<'_>| {
        Ok(Kandidat {
            id: row.get(0)?,
            path: PathBuf::from(row.get::<_, String>(1)?),
            size: row.get(2)?,
            modified: row.get(3)?,
        })
    };
    type Zeilen = std::result::Result<Vec<Kandidat>, rusqlite::Error>;
    let rows: Zeilen = match root_id {
        Some(id) => stmt.query_map(rusqlite::params![min_size, id], abbilden)?.collect(),
        None => stmt.query_map(rusqlite::params![min_size], abbilden)?.collect(),
    };
    Ok(rows?)
}

type HashErgebnis = (i64, Result<String>);

/// Hasht eine einzelne Datei - mit allen Vorbehalten.
///
/// Drei Dinge werden hier verhindert:
///
/// 1. **Cloud-Downloads.** Ein Platzhalter wuerde beim Lesen Gigabytes aus dem
///    Netz holen. FileTriage laesst ihn in Ruhe.
/// 2. **Verknuepfungen.** Ein Symlink wuerde den Inhalt seines Ziels liefern
///    und dieses damit faelschlich als Duplikat ausweisen.
/// 3. **Veraltete Hashes.** Aendert sich die Datei waehrend des Lesens, gehoert
///    der Hash zu keinem existierenden Zustand und wird verworfen.
fn hashe_eine(k: &Kandidat, voll: bool) -> Result<String> {
    let vorher = crate::fsmeta::inspect(&k.path)?;
    if vorher.class != crate::fsmeta::FileClass::Regular {
        return Err(AppError::Invalid(format!(
            "{} ist keine gewoehnliche lokale Datei",
            k.path.display()
        )));
    }

    let hash = if voll { full_hash(&k.path)? } else { quick_hash(&k.path, k.size as u64)? };

    // Nach dem Lesen erneut pruefen: Nur wenn sich in der Zwischenzeit nichts
    // geaendert hat, beschreibt der Hash einen echten Dateizustand.
    let nachher = crate::fsmeta::inspect(&k.path)?;
    if !vorher.fingerprint.matches(&nachher.fingerprint) {
        return Err(AppError::Changed(format!(
            "{} hat sich während der Prüfung verändert",
            k.path.display()
        )));
    }
    // Und passt der gelesene Zustand ueberhaupt zu dem, was im Index steht?
    if nachher.fingerprint.size != k.size || nachher.fingerprint.modified != k.modified {
        return Err(AppError::Changed(format!(
            "{} weicht vom Indexstand ab",
            k.path.display()
        )));
    }
    Ok(hash)
}

fn hashe_parallel(
    kandidaten: &[Kandidat],
    cancel: &AtomicBool,
    voll: bool,
    on_progress: &mut dyn FnMut(u64),
) -> Vec<HashErgebnis> {
    // Der Zaehler laeuft in den Arbeits-Threads mit; gemeldet wird vom
    // aufrufenden Thread, damit der Callback nicht synchronisiert werden muss.
    let zaehler = AtomicU64::new(0);
    let ergebnisse: Vec<HashErgebnis> = kandidaten
        .par_iter()
        .map(|k| {
            if cancel.load(Ordering::Relaxed) {
                return (k.id, Err(AppError::Cancelled));
            }
            let hash = hashe_eine(k, voll);
            zaehler.fetch_add(1, Ordering::Relaxed);
            (k.id, hash)
        })
        .collect();
    on_progress(zaehler.load(Ordering::Relaxed));
    ergebnisse
}

fn schreibe_hashes(
    conn: &mut Connection,
    kandidaten: &[Kandidat],
    ergebnisse: &[HashErgebnis],
    voll: bool,
) -> Result<()> {
    let nach_id: std::collections::HashMap<i64, &Kandidat> =
        kandidaten.iter().map(|k| (k.id, k)).collect();
    let tx = conn.transaction()?;
    {
        let sql = if voll {
            "UPDATE files SET full_hash = ?1, hash_size = ?2, hash_mtime = ?3 WHERE id = ?4"
        } else {
            "UPDATE files SET quick_hash = ?1, hash_size = ?2, hash_mtime = ?3 WHERE id = ?4"
        };
        let mut stmt = tx.prepare(sql)?;
        for (id, hash) in ergebnisse {
            let Ok(hash) = hash else { continue };
            let Some(k) = nach_id.get(id) else { continue };
            stmt.execute(rusqlite::params![hash, k.size, k.modified, id])?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Liefert die fertigen Duplikatgruppen aus dem Index.
pub fn groups(conn: &Connection, root: Option<&str>, min_size: i64) -> Result<Vec<DupGroup>> {
    let root_id = match root {
        Some(r) => crate::scan::root_id_for(conn, r)?,
        None => None,
    };
    let bedingung = if root_id.is_some() { "AND root_id = ?2" } else { "" };
    // `frisch` ist der eigentliche Sicherheitsteil dieser Abfrage: Ein Hash
    // gilt nur, solange Groesse und Zeitstempel noch die sind, unter denen er
    // berechnet wurde. Aendert sich eine Datei nach dem Hashen, verschwindet
    // sie hier - lieber keine Gruppe als eine falsche Behauptung.
    let frisch = "full_hash IS NOT NULL AND hash_size = size AND hash_mtime = modified";
    let sql = format!(
        "SELECT full_hash, size, id, path, parent, name, ext, created, modified, category, state
         FROM files
         WHERE {frisch} AND size >= ?1 {bedingung}
           AND full_hash IN (SELECT full_hash FROM files
                             WHERE {frisch} AND size >= ?1 {bedingung}
                             GROUP BY full_hash HAVING count(*) > 1)
         ORDER BY size DESC, full_hash, path"
    );

    let mut stmt = conn.prepare(&sql)?;
    let abbilden = |row: &rusqlite::Row<'_>| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            FileEntry {
                id: row.get(2)?,
                path: row.get(3)?,
                parent: row.get(4)?,
                name: row.get(5)?,
                ext: row.get(6)?,
                size: row.get(1)?,
                created: row.get(7)?,
                modified: row.get(8)?,
                category: row.get(9)?,
                state: row.get(10)?,
            },
        ))
    };
    let zeilen: Vec<(String, i64, FileEntry)> = match root_id {
        Some(id) => stmt
            .query_map(rusqlite::params![min_size, id], abbilden)?
            .collect::<std::result::Result<_, _>>()?,
        None => stmt
            .query_map(rusqlite::params![min_size], abbilden)?
            .collect::<std::result::Result<_, _>>()?,
    };

    let mut gruppen: Vec<DupGroup> = Vec::new();
    for (hash, size, eintrag) in zeilen {
        match gruppen.last_mut() {
            Some(g) if g.hash == hash => g.files.push(eintrag),
            _ => gruppen.push(DupGroup {
                hash,
                size,
                files: vec![eintrag],
                wasted: 0,
                hardlinked: false,
                verified: true,
            }),
        }
    }
    for g in &mut gruppen {
        // Wie viele *verschiedene* Datenblöcke stecken in der Gruppe? Nur die
        // koennen Platz freigeben. Ohne diese Unterscheidung wuerde FileTriage
        // bei Hardlinks Speicher versprechen, den es nie gibt.
        let mut kennungen = std::collections::HashSet::new();
        let mut ohne_kennung = 0i64;
        for f in &g.files {
            match crate::fsmeta::file_id(std::path::Path::new(&f.path)) {
                Some(id) => {
                    kennungen.insert((id.volume, id.index));
                }
                None => ohne_kennung += 1,
            }
        }
        let eigenstaendig = kennungen.len() as i64 + ohne_kennung;
        g.hardlinked = eigenstaendig < g.files.len() as i64;
        g.wasted = g.size * (eigenstaendig - 1).max(0);
    }
    gruppen.retain(|g| g.files.len() > 1);
    Ok(gruppen)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ScanSettings;
    use crate::scan::{ScanOptions, scan};
    use crate::testutil::TempDir;

    fn vorbereiten(dateien: &[(&str, &[u8])]) -> (TempDir, Db) {
        let dir = TempDir::new("dup");
        for (name, inhalt) in dateien {
            dir.file(name, inhalt);
        }
        let db = Db::open(dir.path().join(".idx.db")).unwrap();
        let opts = ScanOptions { root: dir.path().to_path_buf(), settings: ScanSettings::default() };
        scan(&db, &opts, &AtomicBool::new(false), &mut |_| {}).unwrap();
        (dir, db)
    }

    fn suchen(db: &Db, dir: &TempDir) -> Vec<DupGroup> {
        let root = dir.path().display().to_string();
        find(db, Some(&root), 1, &AtomicBool::new(false), &mut |_| {}).unwrap();
        let conn = db.conn().unwrap();
        groups(&conn, Some(&root), 1).unwrap()
    }

    #[test]
    fn findet_echte_duplikate() {
        let (dir, db) = vorbereiten(&[
            ("a.txt", b"gleicher inhalt"),
            ("unter/b.txt", b"gleicher inhalt"),
            ("c.txt", b"anderer inhalt!"),
        ]);
        let gruppen = suchen(&db, &dir);
        assert_eq!(gruppen.len(), 1);
        assert_eq!(gruppen[0].files.len(), 2);
        assert_eq!(gruppen[0].wasted, "gleicher inhalt".len() as i64);
    }

    #[test]
    fn gleiche_groesse_allein_ist_kein_duplikat() {
        // Beide 8 Bytes, unterschiedlicher Inhalt.
        let (dir, db) = vorbereiten(&[("x.bin", b"AAAAAAAA"), ("y.bin", b"BBBBBBBB")]);
        assert!(suchen(&db, &dir).is_empty());
    }

    #[test]
    fn gleicher_name_in_verschiedenen_ordnern_ist_kein_duplikat() {
        let (dir, db) = vorbereiten(&[("eins/gleich.txt", b"inhalt A"), ("zwei/gleich.txt", b"inhalt B")]);
        assert!(suchen(&db, &dir).is_empty());
    }

    #[test]
    fn grosse_dateien_die_sich_nur_am_ende_unterscheiden() {
        let mut a = vec![7u8; 300 * 1024];
        let mut b = a.clone();
        a.extend_from_slice(b"ENDE-A");
        b.extend_from_slice(b"ENDE-B");
        let (dir, db) = vorbereiten(&[("gross_a.bin", &a), ("gross_b.bin", &b)]);
        assert!(suchen(&db, &dir).is_empty(), "Teil-Hash muss das Ende mitlesen");
    }

    #[test]
    fn drei_gleiche_ergeben_eine_gruppe() {
        let (dir, db) = vorbereiten(&[
            ("1.dat", b"identisch"),
            ("2.dat", b"identisch"),
            ("unter/3.dat", b"identisch"),
        ]);
        let gruppen = suchen(&db, &dir);
        assert_eq!(gruppen.len(), 1);
        assert_eq!(gruppen[0].files.len(), 3);
        assert_eq!(gruppen[0].wasted, 2 * "identisch".len() as i64);
    }

    #[test]
    fn leere_dateien_werden_uebersprungen() {
        let (dir, db) = vorbereiten(&[("leer1.txt", b""), ("leer2.txt", b"")]);
        // min_size = 1 schliesst leere Dateien aus - sie sind kein Platzproblem.
        assert!(suchen(&db, &dir).is_empty());
    }

    #[test]
    fn hashwerte_sind_stabil_und_unterscheiden() {
        let dir = TempDir::new("hash");
        let a = dir.file("a", b"hallo welt");
        let b = dir.file("b", b"hallo welt");
        let c = dir.file("c", b"hallo Welt");
        assert_eq!(full_hash(&a).unwrap(), full_hash(&b).unwrap());
        assert_ne!(full_hash(&a).unwrap(), full_hash(&c).unwrap());
        assert_eq!(quick_hash(&a, 10).unwrap(), quick_hash(&b, 10).unwrap());
    }

    /// Die drei Faelle, an denen sich entscheidet, ob "identisch" die Wahrheit
    /// sagt. Die Suche ist sehr schnell - diese Geschwindigkeit darf niemals
    /// dadurch entstehen, dass die letzte Stufe uebersprungen wird.
    ///
    /// A: gleiche Groesse, anderer Inhalt.
    /// B: gleicher Anfang *und* gleiches Ende, andere Mitte - der Teil-Hash
    ///    kann das nicht unterscheiden, der vollstaendige schon.
    /// C: Byte fuer Byte gleich.
    #[test]
    fn nur_byte_identische_dateien_gelten_als_bestaetigt() {
        // Deutlich groesser als die 64 KiB, die der Teil-Hash je Ende liest.
        let rand = 200 * 1024;
        let mut b1 = vec![1u8; rand];
        b1.extend_from_slice(&vec![0xAA; rand]);
        b1.extend_from_slice(&vec![2u8; rand]);
        let mut b2 = vec![1u8; rand];
        b2.extend_from_slice(&vec![0xBB; rand]);
        b2.extend_from_slice(&vec![2u8; rand]);
        assert_eq!(b1.len(), b2.len());
        assert_eq!(b1[..rand], b2[..rand], "gleicher Anfang");
        assert_eq!(b1[b1.len() - rand..], b2[b2.len() - rand..], "gleiches Ende");
        assert_eq!(
            quick_hash_von_bytes(&b1),
            quick_hash_von_bytes(&b2),
            "Voraussetzung des Tests: der Teil-Hash haelt B fuer gleich"
        );

        let (dir, db) = vorbereiten(&[
            ("A1.bin", b"AAAAAAAA"),
            ("A2.bin", b"BBBBBBBB"),
            ("B1.bin", &b1),
            ("B2.bin", &b2),
            ("C1.bin", b"exakt derselbe Inhalt"),
            ("C2.bin", b"exakt derselbe Inhalt"),
        ]);
        let gruppen = suchen(&db, &dir);

        let namen: Vec<String> = gruppen
            .iter()
            .flat_map(|g| g.files.iter().map(|f| f.name.clone()))
            .collect();
        assert!(!namen.iter().any(|n| n.starts_with('A')), "A: gleiche Groesse ist kein Beweis");
        assert!(
            !namen.iter().any(|n| n.starts_with('B')),
            "B: gleicher Anfang und gleiches Ende sind kein Beweis - {namen:?}"
        );
        assert_eq!(gruppen.len(), 1, "nur C bleibt uebrig: {namen:?}");
        assert_eq!(gruppen[0].files.len(), 2);
        assert!(gruppen[0].verified, "eine Gruppe existiert nur vollstaendig geprueft");
    }

    /// Hilfsfunktion fuer den Test oben: Teil-Hash ueber einen Puffer, ohne
    /// den Umweg ueber eine Datei.
    fn quick_hash_von_bytes(daten: &[u8]) -> String {
        let dir = TempDir::new("qh");
        let p = dir.file("x.bin", daten);
        quick_hash(&p, daten.len() as u64).unwrap()
    }

    /// Ein Hash beschreibt einen Dateizustand, nicht einen Dateinamen. Aendert
    /// sich die Datei nach dem Hashen, darf die alte Zahl nicht weiter als
    /// Beweis herhalten.
    #[test]
    fn veralteter_hash_bildet_keine_gruppe_mehr() {
        let (dir, db) = vorbereiten(&[("a.bin", b"gleich gleich"), ("b.bin", b"gleich gleich")]);
        assert_eq!(suchen(&db, &dir).len(), 1);

        // Der Index bekommt eine neue Groesse, ohne dass neu gehasht wird -
        // genau der Zustand, den ein unterbrochener Scan hinterlassen koennte.
        {
            let conn = db.conn().unwrap();
            conn.execute("UPDATE files SET size = size + 1 WHERE name = 'a.bin'", []).unwrap();
        }
        let conn = db.conn().unwrap();
        let root = dir.path().display().to_string();
        assert!(
            groups(&conn, Some(&root), 1).unwrap().is_empty(),
            "ein Hash, der nicht mehr zum Dateizustand passt, beweist nichts"
        );
    }

    #[test]
    fn fehlende_datei_liefert_fehler_statt_panik() {
        let dir = TempDir::new("hash-missing");
        let err = full_hash(&dir.path().join("weg.bin")).unwrap_err();
        assert_eq!(err.kind(), "not_found");
    }
}
