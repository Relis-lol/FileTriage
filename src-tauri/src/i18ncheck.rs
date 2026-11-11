//! Prüfungen der Sprachkataloge.
//!
//! Eine Übersetzung fällt nicht auf, wenn sie fehlt — sie fällt auf, wenn ein
//! Nutzer sie braucht. Deshalb wird hier geprüft, was sich prüfen lässt:
//!
//!   * Jeder Katalog hat jeden Schlüssel, den Englisch hat.
//!   * Kein Katalog hat Schlüssel, die es sonst nirgends gibt.
//!   * Die Platzhalter stimmen überein. Eine Übersetzung, die `{count}`
//!     verschluckt, zeigt später eine Meldung ohne Zahl.
//!   * Jede Mengenangabe hat die Form `other` — die gibt es in jeder Sprache
//!     und ist die Rückfallform.
//!   * Ein heuristischer Advisory-Scan meldet typische sichtbare Literale im
//!     JavaScript. Null Treffer sind kein Vollstaendigkeitsbeweis.
//!
//! Was diese Tests **nicht** prüfen: ob eine Übersetzung gut ist. Das kann
//! nur ein Mensch, der die Sprache spricht.

#![cfg(test)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

fn ui_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join("ui")
}

fn locales_dir() -> PathBuf {
    ui_dir().join("locales")
}

/// Alle vorhandenen Kataloge, nach Sprachkennung.
fn kataloge() -> BTreeMap<String, serde_json::Value> {
    let mut out = BTreeMap::new();
    for e in std::fs::read_dir(locales_dir()).expect("locales fehlt").flatten() {
        let p = e.path();
        if p.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let name = p.file_stem().unwrap().to_string_lossy().to_string();
        let roh = std::fs::read_to_string(&p).unwrap();
        let wert: serde_json::Value = serde_json::from_str(&roh)
            .unwrap_or_else(|e| panic!("{name}.json ist kein gültiges JSON: {e}"));
        out.insert(name, wert);
    }
    out
}

fn schluessel(v: &serde_json::Value) -> BTreeSet<String> {
    v.as_object().expect("Katalog ist kein Objekt").keys().cloned().collect()
}

/// Die Platzhalter eines Eintrags — `{count}`, `{name}`, …
fn platzhalter(v: &serde_json::Value) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut sammle = |s: &str| {
        let bytes = s.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'{' {
                if let Some(ende) = s[i..].find('}') {
                    out.insert(s[i + 1..i + ende].to_string());
                    i += ende;
                }
            }
            i += 1;
        }
    };
    match v {
        serde_json::Value::String(s) => sammle(s),
        serde_json::Value::Object(o) => {
            for w in o.values() {
                if let Some(s) = w.as_str() {
                    sammle(s);
                }
            }
        }
        _ => {}
    }
    out
}

// ---------------------------------------------------------------------------

#[test]
fn englisch_ist_vorhanden_und_ist_die_rueckfallebene() {
    let alle = kataloge();
    assert!(alle.contains_key("en"), "en.json fehlt — es gibt keine Rückfallebene");
    assert!(!schluessel(&alle["en"]).is_empty(), "en.json ist leer");
}

/// Jeder mitgelieferte Katalog muss jeden Schlüssel haben.
///
/// Ein fehlender Schlüssel ist kein Absturz — die Oberfläche fällt Satz für
/// Satz auf Englisch zurück. Aber eine halb übersetzte Oberfläche ist
/// schlechter als eine englische, und wer sie ausliefert, sollte das
/// wenigstens wissen.
#[test]
fn jeder_katalog_ist_vollstaendig() {
    let alle = kataloge();
    let referenz = schluessel(&alle["en"]);

    for (sprache, katalog) in &alle {
        if sprache == "en" {
            continue;
        }
        let vorhanden = schluessel(katalog);
        let fehlend: Vec<_> = referenz.difference(&vorhanden).take(10).collect();
        assert!(
            fehlend.is_empty(),
            "{sprache}.json fehlen {} Schlüssel, darunter: {fehlend:?}",
            referenz.difference(&vorhanden).count()
        );

        let ueberzaehlig: Vec<_> = vorhanden.difference(&referenz).take(10).collect();
        assert!(
            ueberzaehlig.is_empty(),
            "{sprache}.json hat Schlüssel, die es in en.json nicht gibt: {ueberzaehlig:?}"
        );
    }
}

/// Die Platzhalter müssen überall dieselben sein.
///
/// Verschluckt eine Übersetzung `{count}`, steht dort später eine Meldung
/// ohne Zahl. Erfindet sie `{files}`, bleibt die geschweifte Klammer im Text
/// stehen.
#[test]
fn die_platzhalter_stimmen_ueberein() {
    let alle = kataloge();
    let en = alle["en"].as_object().unwrap();

    for (sprache, katalog) in &alle {
        if sprache == "en" {
            continue;
        }
        for (schluessel, wert) in katalog.as_object().unwrap() {
            let Some(vorbild) = en.get(schluessel) else { continue };
            let erwartet = platzhalter(vorbild);
            let tatsaechlich = platzhalter(wert);
            assert_eq!(
                erwartet, tatsaechlich,
                "{sprache}.json / {schluessel}: Platzhalter weichen ab"
            );
        }
    }
}

/// Mengenangaben brauchen mindestens die Form `other`.
#[test]
fn jede_mengenangabe_hat_eine_rueckfallform() {
    for (sprache, katalog) in kataloge() {
        for (schluessel, wert) in katalog.as_object().unwrap() {
            if let serde_json::Value::String(text) = wert {
                assert!(
                    !text.contains("{count}"),
                    "{sprache}.json / {schluessel}: Mengenangabe ist ein Skalar und kann Singular/Plural nicht unterscheiden"
                );
                continue;
            }
            let serde_json::Value::Object(formen) = wert else { continue };
            assert!(
                formen.contains_key("other"),
                "{sprache}.json / {schluessel}: keine Form \"other\""
            );
            for (form, text) in formen {
                assert!(
                    text.is_string(),
                    "{sprache}.json / {schluessel} / {form}: kein Text"
                );
            }
        }
    }
}

/// Die Liste der angebotenen Sprachen und die vorhandenen Dateien müssen
/// zusammenpassen. Eine Sprache anzubieten, für die es keinen Katalog gibt,
/// wäre ein leeres Versprechen.
#[test]
fn angebotene_sprachen_haben_einen_katalog() {
    let i18n = std::fs::read_to_string(ui_dir().join("lib/i18n.js")).unwrap();
    let tabelle = i18n
        .split("export const SPRACHEN = {")
        .nth(1)
        .and_then(|s| s.split("};").next())
        .expect("SPRACHEN fehlt");

    let mut angeboten = Vec::new();
    for zeile in tabelle.lines() {
        let t = zeile.trim();
        let Some(pos) = t.find(':') else { continue };
        let name = t[..pos].trim().trim_matches('\'').trim_matches('"');
        if !name.is_empty() && !name.starts_with("//") {
            angeboten.push(name.to_string());
        }
    }
    assert!(angeboten.len() >= 2, "die Sprachliste ist leer: {angeboten:?}");

    let vorhanden: BTreeSet<String> = kataloge().keys().cloned().collect();
    let ohne_katalog: Vec<_> =
        angeboten.iter().filter(|s| !vorhanden.contains(*s)).cloned().collect();
    assert!(
        ohne_katalog.is_empty(),
        "angeboten, aber ohne Katalog: {ohne_katalog:?} — entweder Katalog \
         nachliefern oder aus SPRACHEN entfernen"
    );
}

/// Arabisch muss als Sprache von rechts nach links geführt sein.
#[test]
fn rechts_nach_links_ist_hinterlegt() {
    let i18n = std::fs::read_to_string(ui_dir().join("lib/i18n.js")).unwrap();
    assert!(i18n.contains("export const RTL"), "keine RTL-Liste");
    let liste = i18n.split("export const RTL").nth(1).unwrap().split(']').next().unwrap();
    assert!(liste.contains("'ar'"), "Arabisch fehlt in der RTL-Liste");
    assert!(
        i18n.contains("document.documentElement.dir"),
        "die Schreibrichtung wird nicht am Dokument gesetzt"
    );
}

/// Der projektspezifische Lexer bleibt bewusst advisory. Ein echter
/// ECMAScript-Parser ist nicht Teil der Werkzeugkette; deshalb darf "0" hier
/// niemals als Beweis fuer vollstaendige UI-Abdeckung gelten.
#[test]
fn heuristischer_string_audit_laeuft_als_advisory_pruefung() {
    let ausgabe = string_audit(&["--advisory", "--count"]);
    assert!(ausgabe.status.success(), "String-Audit konnte nicht laufen:\n{}\n{}",
        String::from_utf8_lossy(&ausgabe.stdout), String::from_utf8_lossy(&ausgabe.stderr));
    assert!(String::from_utf8_lossy(&ausgabe.stdout).trim().parse::<usize>().is_ok());
}

#[test]
fn string_audit_erkennt_argumente_templates_ternaries_fallbacks_und_konkatenation() {
    let ausgabe = string_audit(&["--self-test"]);
    assert!(ausgabe.status.success(), "String-Audit-Selbsttest fehlgeschlagen:\n{}\n{}",
        String::from_utf8_lossy(&ausgabe.stdout), String::from_utf8_lossy(&ausgabe.stderr));
}

fn string_audit(args: &[&str]) -> std::process::Output {
    let script = ui_dir().parent().unwrap().join("tools/extract_strings.py");
    for python in ["python", "python3"] {
        if let Ok(output) = std::process::Command::new(python).arg(&script).args(args).output() {
            return output;
        }
    }
    panic!("Python fuer tools/extract_strings.py nicht gefunden")
}

/// Deutsch und Englisch sind die Sprachen, die ein Mensch geprüft hat. Sie
/// müssen deshalb wirklich vollständig sein, nicht nur formal.
#[test]
fn deutsch_und_englisch_sind_vollstaendig() {
    let alle = kataloge();
    for sprache in ["de", "en"] {
        let katalog = alle.get(sprache).unwrap_or_else(|| panic!("{sprache}.json fehlt"));
        let anzahl = schluessel(katalog).len();
        assert!(anzahl > 250, "{sprache}.json hat nur {anzahl} Schlüssel");

        for (schluessel, wert) in katalog.as_object().unwrap() {
            let leer = match wert {
                serde_json::Value::String(s) => s.trim().is_empty(),
                serde_json::Value::Object(o) => {
                    o.values().any(|w| w.as_str().is_none_or(|s| s.trim().is_empty()))
                }
                _ => true,
            };
            assert!(!leer, "{sprache}.json / {schluessel} ist leer");
        }
    }
}
