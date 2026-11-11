//! Chaos-Test: zufaellige Operationsfolgen gegen harte Invarianten.
//!
//! Die Idee ist nicht, einen bestimmten Fehler zu suchen, sondern FileTriage
//! so lange durcheinanderzubringen, bis eine Invariante bricht. Nach **jedem**
//! Schritt wird geprueft:
//!
//! > Jeder Dateiinhalt, der einmal angelegt und nicht ausdruecklich entfernt
//! > wurde, ist irgendwo auf der Platte auffindbar.
//!
//! Das ist die praktische Fassung von "keine Datei geht verloren". Sie gilt
//! auch mitten in einem abgestuerzten Verschiebevorgang: Dann liegt der Inhalt
//! eben an der Quelle, am Ziel oder in einer Zwischendatei - aber er liegt
//! irgendwo.
//!
//! Jeder Lauf hat einen Seed. Schlaegt ein Lauf fehl, gibt der Test Seed und
//! vollstaendige Operationsfolge aus, damit er sich exakt wiederholen laesst.

#![cfg(test)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::config::DeleteMode;
use crate::db::Db;
use crate::fileops::{self, CollisionPolicy, OpStatus, crashpoint};
use crate::recovery;
use crate::testutil::TempDir;

/// Kleiner, reproduzierbarer Zufallszahlengenerator (xorshift64*).
/// Bewusst selbstgebaut: Fuer Testdaten braucht es keine Abhaengigkeit, und
/// der Seed muss exakt reproduzierbar sein.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed.max(1))
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { (self.next() % n as u64) as usize }
    }

    fn chance(&mut self, prozent: u64) -> bool {
        self.next() % 100 < prozent
    }
}

/// Zustand, den der Test ueber die Wirklichkeit fuehrt.
struct Modell {
    /// Inhalte, die auffindbar sein muessen.
    erwartet: BTreeSet<String>,
    /// Inhalte, die bewusst im Papierkorb gelandet sind.
    im_papierkorb: BTreeMap<i64, String>,
    /// Alles, was der Test jemals angelegt hat.
    ///
    /// Nach einem abgebrochenen Verschiebevorgang darf derselbe Inhalt
    /// voelling zu Recht an zwei Stellen liegen (Original und geprueifte
    /// Kopie). Deshalb kann "muss vorhanden sein" nicht dieselbe Menge sein
    /// wie "darf vorhanden sein".
    jemals_erzeugt: BTreeSet<String>,
    zaehler: usize,
}

const CRASHPOINTS: [&str; 6] = [
    "nach_plan",
    "vor_rename",
    "nach_copying_markiert",
    "nach_kopie",
    "nach_verifikation",
    "vor_quelle_loeschen",
];

struct Chaos {
    rng: Rng,
    dir: TempDir,
    conn: Connection,
    ordner: Vec<PathBuf>,
    modell: Modell,
    protokoll: Vec<String>,
    seed: u64,
}

impl Chaos {
    fn new(seed: u64) -> Chaos {
        let dir = TempDir::new(&format!("chaos-{seed}"));
        // Der Index liegt ausserhalb des Spielfelds.
        let db = Db::open(dir.path().join("index").join("i.db")).unwrap();
        let conn = db.conn().unwrap();
        let ordner = vec![
            dir.dir("arbeit"),
            dir.dir("ziel-a"),
            dir.dir("ziel-b"),
            dir.dir("ziel-b/tiefer"),
        ];
        Chaos {
            rng: Rng::new(seed),
            dir,
            conn,
            ordner,
            modell: Modell {
                erwartet: BTreeSet::new(),
                im_papierkorb: BTreeMap::new(),
                jemals_erzeugt: BTreeSet::new(),
                zaehler: 0,
            },
            protokoll: Vec::new(),
            seed,
        }
    }

    fn log(&mut self, s: String) {
        self.protokoll.push(s);
    }

    /// Alle Dateien im Spielfeld (ohne den Indexordner).
    fn dateien(&self) -> Vec<PathBuf> {
        let mut gefunden = Vec::new();
        let index = self.dir.path().join("index");
        sammle(self.dir.path(), &index, &mut gefunden);
        gefunden
    }

    /// Alle auffindbaren Inhalte - inklusive Zwischendateien.
    fn inhalte(&self) -> BTreeMap<String, usize> {
        let mut m: BTreeMap<String, usize> = BTreeMap::new();
        for p in self.dateien() {
            if let Ok(inhalt) = std::fs::read_to_string(&p) {
                *m.entry(inhalt).or_insert(0) += 1;
            }
        }
        m
    }

    fn zufalls_datei(&mut self) -> Option<PathBuf> {
        let mut d = self.dateien();
        // Zwischendateien sind kein gueltiges Ziel fuer Nutzeraktionen.
        d.retain(|p| {
            !crate::journal::is_temp_name(&p.file_name().unwrap_or_default().to_string_lossy())
        });
        if d.is_empty() {
            return None;
        }
        let i = self.rng.below(d.len());
        Some(d[i].clone())
    }

    fn zufalls_ordner(&mut self) -> PathBuf {
        let i = self.rng.below(self.ordner.len());
        self.ordner[i].clone()
    }

    /// **Die** Invariante.
    fn pruefe_invarianten(&self, schritt: usize) {
        let vorhanden = self.inhalte();
        for inhalt in &self.modell.erwartet {
            let anzahl = vorhanden.get(inhalt).copied().unwrap_or(0);
            assert!(
                anzahl >= 1,
                "\n\nDATENVERLUST in Schritt {schritt} (Seed {})\n\
                 Inhalt nicht mehr auffindbar: {inhalt:?}\n\
                 Operationsfolge:\n{}\n",
                self.seed,
                self.protokoll.join("\n")
            );
        }
        // Nichts darf aus dem Nichts entstehen: Jeder Inhalt auf der Platte
        // muss irgendwann von diesem Test geschrieben worden sein. Das faengt
        // beschaedigte oder halb geschriebene Dateien.
        for inhalt in vorhanden.keys() {
            assert!(
                self.modell.jemals_erzeugt.contains(inhalt),
                "\n\nUnbekannter oder beschaedigter Inhalt in Schritt {schritt} (Seed {}): \
                 {inhalt:?}\n{}\n",
                self.seed,
                self.protokoll.join("\n")
            );
        }
    }

    fn schritt(&mut self, nr: usize) {
        let wahl = self.rng.below(100);
        match wahl {
            // ---- neue Datei anlegen ------------------------------------
            0..=24 => {
                self.modell.zaehler += 1;
                let inhalt = format!("inhalt-{}-{}", self.seed, self.modell.zaehler);
                let ordner = self.zufalls_ordner();
                let name = format!("datei-{}.txt", self.modell.zaehler);
                let pfad = ordner.join(&name);
                std::fs::write(&pfad, &inhalt).unwrap();
                self.modell.jemals_erzeugt.insert(inhalt.clone());
                self.modell.erwartet.insert(inhalt);
                self.log(format!("{nr}: anlegen {}", pfad.display()));
            }

            // ---- verschieben -------------------------------------------
            25..=49 => {
                let Some(src) = self.zufalls_datei() else { return };
                let ziel = self.zufalls_ordner();
                let policy =
                    if self.rng.chance(50) { CollisionPolicy::Rename } else { CollisionPolicy::Skip };
                let ueber_volume = self.rng.chance(40);
                let _reset = crashpoint::Reset;
                crashpoint::force_copy_path(ueber_volume);
                let r = fileops::move_file(&self.conn, &src, &ziel, policy, None);
                self.log(format!(
                    "{nr}: verschieben {} -> {} ({:?}, kopie={ueber_volume}) = {:?}",
                    src.display(),
                    ziel.display(),
                    policy,
                    r.status
                ));
            }

            // ---- verschieben mit Absturz -------------------------------
            50..=61 => {
                let Some(src) = self.zufalls_datei() else { return };
                let ziel = self.zufalls_ordner();
                let punkt = CRASHPOINTS[self.rng.below(CRASHPOINTS.len())];
                let ueber_volume = self.rng.chance(60);
                let _reset = crashpoint::Reset;
                crashpoint::force_copy_path(ueber_volume);
                crashpoint::arm(punkt);
                let r = fileops::move_file(&self.conn, &src, &ziel, CollisionPolicy::Rename, None);
                self.log(format!(
                    "{nr}: ABSTURZ bei {punkt} beim Verschieben {} -> {} (kopie={ueber_volume}) = {:?}",
                    src.display(),
                    ziel.display(),
                    r.status
                ));
            }

            // ---- umbenennen ---------------------------------------------
            62..=71 => {
                let Some(src) = self.zufalls_datei() else { return };
                self.modell.zaehler += 1;
                let neu = format!("umbenannt-{}.txt", self.modell.zaehler);
                let r = fileops::rename_file(&self.conn, &src, &neu);
                self.log(format!("{nr}: umbenennen {} -> {neu} = {:?}", src.display(), r.status));
            }

            // ---- in den Papierkorb --------------------------------------
            72..=79 => {
                let Some(src) = self.zufalls_datei() else { return };
                let Ok(inhalt) = std::fs::read_to_string(&src) else { return };
                let r = fileops::delete_file(&self.conn, &src, DeleteMode::Recycle, None);
                self.log(format!("{nr}: papierkorb {} = {:?}", src.display(), r.status));
                if r.status == OpStatus::Done {
                    // Der Inhalt darf jetzt fehlen - er liegt im Papierkorb.
                    self.modell.erwartet.remove(&inhalt);
                    self.modell.im_papierkorb.insert(r.action_id.unwrap_or(0), inhalt);
                }
            }

            // ---- rueckgaengig -------------------------------------------
            80..=89 => {
                let vorher = crate::journal::undoable(&self.conn, 1).unwrap();
                let Some(aktion) = vorher.into_iter().next() else { return };
                let war_papierkorb = aktion.kind == "delete_recycle";
                let u = crate::undo::undo(&self.conn, aktion.id);
                self.log(format!(
                    "{nr}: rueckgaengig #{} ({}) = {}",
                    aktion.id,
                    aktion.kind,
                    if u.ok { "ok" } else { "abgelehnt" }
                ));
                if u.ok && war_papierkorb {
                    if let Some(inhalt) = self.modell.im_papierkorb.remove(&aktion.id) {
                        self.modell.jemals_erzeugt.insert(inhalt.clone());
                        self.modell.erwartet.insert(inhalt);
                    }
                }
            }

            // ---- Datei von aussen veraendern ----------------------------
            90..=94 => {
                let Some(src) = self.zufalls_datei() else { return };
                let Ok(alt) = std::fs::read_to_string(&src) else { return };
                self.modell.zaehler += 1;
                let neu = format!("inhalt-{}-{}", self.seed, self.modell.zaehler);
                // Kurz warten, damit der Zeitstempel sich unterscheidet.
                std::thread::sleep(std::time::Duration::from_millis(3));
                std::fs::write(&src, &neu).unwrap();
                self.modell.erwartet.remove(&alt);
                self.modell.im_papierkorb.retain(|_, v| v != &alt);
                self.modell.jemals_erzeugt.insert(neu.clone());
                self.modell.erwartet.insert(neu);
                self.log(format!("{nr}: von aussen veraendert {}", src.display()));
            }

            // ---- Wiederherstellung laufen lassen ------------------------
            _ => {
                let bericht = recovery::inspect(&self.conn, true).unwrap();
                let mut aufgeloest = 0;
                for item in &bericht.items {
                    // Immer die erste angebotene Wahl nehmen - auch die
                    // datenveraendernden. Genau die sollen geprueft werden.
                    if let Some(wahl) = item.choices.first() {
                        if recovery::resolve(&self.conn, item.action_id, &wahl.id).is_ok() {
                            aufgeloest += 1;
                        }
                    }
                }
                self.log(format!(
                    "{nr}: wiederherstellung ({} automatisch, {} gemeldet, {aufgeloest} aufgeloest)",
                    bericht.auto_resolved, bericht.needs_attention
                ));
            }
        }
    }
}

fn sammle(dir: &Path, ausnahme: &Path, out: &mut Vec<PathBuf>) {
    let Ok(eintraege) = std::fs::read_dir(dir) else { return };
    for e in eintraege.flatten() {
        let p = e.path();
        if p == ausnahme {
            continue;
        }
        match e.file_type() {
            Ok(t) if t.is_dir() => sammle(&p, ausnahme, out),
            Ok(t) if t.is_file() => out.push(p),
            _ => {}
        }
    }
}

fn lauf(seed: u64, schritte: usize) {
    let mut c = Chaos::new(seed);
    for i in 0..schritte {
        c.schritt(i);
        c.pruefe_invarianten(i);
    }

    // Am Ende alles aufloesen und noch einmal pruefen: Auch nach der
    // Wiederherstellung darf nichts fehlen.
    let bericht = recovery::inspect(&c.conn, true).unwrap();
    for item in &bericht.items {
        if let Some(wahl) = item.choices.first() {
            let _ = recovery::resolve(&c.conn, item.action_id, &wahl.id);
        }
    }
    c.pruefe_invarianten(schritte);
}

#[test]
fn chaos_kurze_laeufe_ueber_viele_seeds() {
    // Viele verschiedene Startpunkte, damit moeglichst unterschiedliche
    // Verschraenkungen entstehen.
    for seed in 1..=60u64 {
        lauf(seed, 40);
    }
}

#[test]
fn chaos_lange_laeufe() {
    for seed in [4242u64, 991, 20260811] {
        lauf(seed, 600);
    }
}

/// Ein Lauf, der ausschliesslich Abstuerze und Wiederherstellung mischt.
#[test]
fn chaos_nur_abstuerze_und_wiederherstellung() {
    for seed in 1..=25u64 {
        let mut c = Chaos::new(seed * 7 + 1);
        // Grundbestand anlegen.
        for i in 0..6 {
            c.modell.zaehler += 1;
            let inhalt = format!("inhalt-{}-{}", c.seed, c.modell.zaehler);
            let ordner = c.zufalls_ordner();
            std::fs::write(ordner.join(format!("start-{i}.txt")), &inhalt).unwrap();
            c.modell.jemals_erzeugt.insert(inhalt.clone());
            c.modell.erwartet.insert(inhalt);
        }

        for i in 0..50 {
            let Some(src) = c.zufalls_datei() else { break };
            let ziel = c.zufalls_ordner();
            let punkt = CRASHPOINTS[c.rng.below(CRASHPOINTS.len())];
            let ueber_volume = c.rng.chance(50);
            {
                let _reset = crashpoint::Reset;
                crashpoint::force_copy_path(ueber_volume);
                crashpoint::arm(punkt);
                fileops::move_file(&c.conn, &src, &ziel, CollisionPolicy::Rename, None);
            }
            c.log(format!("{i}: ABSTURZ bei {punkt} (kopie={ueber_volume})"));
            c.pruefe_invarianten(i);

            let bericht = recovery::inspect(&c.conn, true).unwrap();
            for item in &bericht.items {
                if let Some(wahl) = item.choices.first() {
                    let _ = recovery::resolve(&c.conn, item.action_id, &wahl.id);
                }
            }
            c.pruefe_invarianten(i);
        }
    }
}

