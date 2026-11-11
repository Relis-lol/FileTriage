//! Prüfungen am ausgelieferten Stand.
//!
//! Diese Tests beantworten eine einzige Frage: Steckt in dem, was den Rechner
//! verlässt, etwas, das dort nicht hingehört? Sie lesen dafür die eigenen
//! Quelldateien und — wenn eines vorliegt — das fertige Release-Programm.
//!
//! Sie ersetzen keine Prüfung durch einen Menschen. Sie verhindern aber, dass
//! ein Schalter, den es in einem verkauften Programm nicht geben darf, still
//! wieder hineinrutscht.

#![cfg(test)]

use std::path::{Path, PathBuf};

fn wurzel() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn lies(rel: &str) -> String {
    let p = wurzel().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{} nicht lesbar: {e}", p.display()))
}

/// Das fertige Release-Programm, falls es eines gibt.
fn release_exe() -> Option<PathBuf> {
    let p = wurzel().join("target").join("release").join("filetriage.exe");
    p.is_file().then_some(p)
}

// ---------------------------------------------------------------------------
// Lizenz
// ---------------------------------------------------------------------------

/// Der umschaltbare Lizenzanbieter darf in einem verkauften Programm nicht
/// existieren. Nicht "deaktiviert" — nicht vorhanden.
#[test]
fn der_entwicklungsanbieter_haengt_am_richtigen_schalter() {
    let src = lies("src/license.rs");
    let gate = "#[cfg(any(debug_assertions, feature = \"dev-license\"))]";

    // Der Modul-Block.
    let pos = src.find("pub mod dev {").expect("Modul dev fehlt");
    let davor = &src[..pos];
    assert!(
        davor.trim_end().ends_with(gate),
        "das Modul dev haengt nicht am Entwicklungsschalter"
    );

    // Und die Stelle, an der er ausgewaehlt wird.
    let pos = src.find("FILETRIAGE_LICENSE").expect("Umgebungsvariable fehlt");
    let umfeld = &src[pos.saturating_sub(600)..pos];
    assert!(
        umfeld.contains(gate),
        "die Umgebungsvariable wird ausserhalb des Entwicklungsschalters gelesen"
    );
}

/// Die Testvorgabe darf es nur unter `cfg(test)` geben.
#[test]
fn die_testvorgabe_existiert_nur_in_tests() {
    let src = lies("src/license.rs");
    for name in ["fn test_override", "fn mit_zustand", "static TESTZUSTAND"] {
        let pos = src.find(name).unwrap_or_else(|| panic!("{name} nicht gefunden"));
        let davor = &src[pos.saturating_sub(400)..pos];
        assert!(
            davor.contains("#[cfg(test)]"),
            "{name} haengt nicht an cfg(test)"
        );
    }
}

/// Das Merkmal `dev-license` darf nicht voreingestellt sein — sonst wäre der
/// Schalter in jedem Bau enthalten.
#[test]
fn dev_license_ist_kein_standardmerkmal() {
    let toml = lies("Cargo.toml");
    let features = toml.split("[features]").nth(1).unwrap_or("");
    let default = features
        .lines()
        .find(|l| l.trim_start().starts_with("default"))
        .unwrap_or("default = []");
    assert!(
        !default.contains("dev-license"),
        "dev-license steht in den Standardmerkmalen: {default}"
    );
}

/// Der Vertriebskanal wird beim Bauen festgelegt, nicht zur Laufzeit gelesen.
/// Ein Schalter, der sich nachträglich umlegen lässt, wäre die Hintertür, die
/// es nicht geben soll.
#[test]
fn der_kanal_steht_zur_bauzeit_fest() {
    let src = lies("src/license.rs");
    assert!(
        src.contains("option_env!(\"FILETRIAGE_CHANNEL\")"),
        "der Kanal wird nicht zur Bauzeit eingebrannt"
    );
    assert!(
        !src.contains("std::env::var(\"FILETRIAGE_CHANNEL\")"),
        "der Kanal wird zur Laufzeit gelesen — das waere umschaltbar"
    );
    // Der oeffentliche Quellstand baut ohne Zusatzparameter als Community.
    assert!(
        src.contains("_ => Channel::Community"),
        "ohne Angabe muss der Source-Available-Bau Community sein"
    );
}

#[test]
fn community_ist_unbegrenzt_und_ohne_store_abhaengigkeit() {
    let src = lies("src/license.rs");
    assert!(src.contains("Channel::Community"));
    assert!(src.contains("LicenseState::Community"));
    assert!(src.contains("CommunityLicenseProvider"));
    let build = lies("build.rs");
    assert!(build.contains("unwrap_or_else(|_| \"community\".into())"));
}

#[test]
fn community_kann_keinen_store_kauf_oeffnen() {
    let commands = lies("src/commands.rs");
    let start = commands.find("pub fn open_purchase").expect("open_purchase fehlt");
    let body = &commands[start..commands.len().min(start + 900)];
    assert!(body.contains("CHANNEL != crate::license::Channel::Store"));
}

#[test]
fn community_ui_zeigt_keine_trial_oder_kaufaufforderung() {
    let project = wurzel();
    let root = project.parent().unwrap();
    let app = std::fs::read_to_string(root.join("ui/app.js")).unwrap();
    let triage = std::fs::read_to_string(root.join("ui/views/triage.js")).unwrap();
    let settings = std::fs::read_to_string(root.join("ui/views/settings.js")).unwrap();
    assert!(app.contains("store.lizenz?.channel === 'community'"));
    assert!(triage.contains("store.lizenz?.channel !== 'community'"));
    assert!(settings.contains("community.license"));
    assert!(settings.contains("kontakt@saveroq.com"));
}

/// Die Oberfläche darf den Lizenzzustand zeigen, aber nicht setzen.
#[test]
fn die_oberflaeche_kann_die_lizenz_nicht_umschalten() {
    let ui = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join("ui");
    let mut gefunden = Vec::new();
    for eintrag in walk(&ui) {
        let inhalt = std::fs::read_to_string(&eintrag).unwrap_or_default();
        for verboten in [
            "set_license",
            "setLicense",
            "unlock",
            "FILETRIAGE_LICENSE",
            "dev_license",
            "devLicense",
        ] {
            if inhalt.contains(verboten) {
                gefunden.push(format!("{}: {verboten}", eintrag.display()));
            }
        }
    }
    assert!(gefunden.is_empty(), "Lizenzschalter in der Oberflaeche: {gefunden:?}");
}

/// Und es gibt keinen Befehl, der ihn setzen könnte.
#[test]
fn es_gibt_keinen_befehl_zum_setzen_der_lizenz() {
    let cmds = lies("src/commands.rs");
    for verboten in ["fn set_license", "fn unlock", "fn activate_license"] {
        assert!(!cmds.contains(verboten), "{verboten} existiert");
    }
}

#[test]
fn produktiver_start_laed_den_lizenzzustand_vor_der_installation() {
    let src = lies("src/lib.rs");
    let laden = src
        .find("license_service.laden(&conn, db.is_new_install())")
        .expect("Lizenzcache wird nicht geladen");
    let install = src.find("license::install(license_service)").expect("Lizenzdienst wird nicht installiert");
    assert!(laden < install, "Lizenzdienst wird installiert, bevor sein Zustand geladen ist");
}

// ---------------------------------------------------------------------------
// Das fertige Programm
// ---------------------------------------------------------------------------

/// Im fertigen Programm darf der Name des Entwicklungsschalters nicht
/// vorkommen. Steht er drin, wurde mit `dev-license` gebaut.
#[test]
fn das_release_programm_kennt_den_entwicklungsschalter_nicht() {
    let Some(exe) = release_exe() else {
        eprintln!("kein Release-Programm vorhanden — uebersprungen");
        return;
    };
    let daten = std::fs::read(&exe).unwrap();
    assert!(
        !enthaelt(&daten, b"FILETRIAGE_LICENSE"),
        "{} enthaelt den Entwicklungsschalter",
        exe.display()
    );
}

/// Und keine Spuren des Rechners, auf dem es gebaut wurde.
///
/// Dieser Test hat beim ersten Lauf zugeschlagen: Der Pfad des
/// Benutzerprofils und der der Crate-Registry standen im Programm.
#[test]
fn das_release_programm_verraet_nicht_wo_es_gebaut_wurde() {
    let Some(exe) = release_exe() else {
        eprintln!("kein Release-Programm vorhanden — uebersprungen");
        return;
    };
    let daten = std::fs::read(&exe).unwrap();
    // Gesucht wird ein echter Profilpfad: "C:\Users\\" plus Kontoname plus
    // Trenner. Der blosse Praefix genuegt nicht - er steht auch in einem
    // Kommentar in styles.css, der als Beispiel erklaert, warum Pfade in
    // einer Rechts-nach-links-Oberflaeche links nach rechts bleiben. Dass
    // Kommentare der Oberflaeche im Programm landen, ist kein Leck, aber es
    // darf keinen Test rot faerben.
    let bs = b'\\';
    let mut praefix = b"c:".to_vec();
    praefix.push(bs);
    praefix.extend_from_slice(b"users");
    praefix.push(bs);

    let klein: Vec<u8> = daten.iter().map(|b| b.to_ascii_lowercase()).collect();
    let mut funde = Vec::new();
    for start in 0..klein.len().saturating_sub(praefix.len()) {
        if klein[start..start + praefix.len()] != praefix[..] {
            continue;
        }
        let rest = &klein[start + praefix.len()..];
        // Ein Kontoname: Buchstaben und Ziffern, dann ein Trenner.
        let name: Vec<u8> = rest
            .iter()
            .take(24)
            .take_while(|c| c.is_ascii_alphanumeric() || **c == b'-' || **c == b'~' || **c == b'.')
            .copied()
            .collect();
        if name.len() >= 3 && rest.get(name.len()) == Some(&bs) {
            funde.push(String::from_utf8_lossy(&name).to_string());
        }
    }
    funde.sort();
    funde.dedup();
    assert!(
        funde.is_empty(),
        "{} enthaelt Pfade aus einem Benutzerprofil: {funde:?}.          tools/build-release.ps1 setzt die --remap-path-prefix-Flags; wurde          ohne dieses Skript gebaut?",
        exe.display()
    );
}

/// Das Programm bringt keinen Netzwerkstapel mit.
///
/// Das ist die belastbarste Fassung der Aussage "FileTriage verbindet sich
/// nicht": Nicht "wir rufen nichts auf", sondern "die Bibliotheken, mit denen
/// man etwas aufrufen könnte, sind nicht eingebunden". Wer das ändern will,
/// bricht diesen Test.
///
/// `ws2_32.dll` ist Windows Sockets, `winhttp` und `wininet` die beiden
/// HTTP-Schichten von Windows. Ohne sie gibt es keinen Weg ins Netz, der ohne
/// weiteres Nachladen auskäme.
#[test]
fn das_release_programm_bindet_keinen_netzwerkstapel_ein() {
    let Some(exe) = release_exe() else {
        eprintln!("kein Release-Programm vorhanden — uebersprungen");
        return;
    };
    let daten = std::fs::read(&exe).unwrap();
    let klein: Vec<u8> = daten.iter().map(|b| b.to_ascii_lowercase()).collect();

    for bibliothek in [
        &b"ws2_32.dll"[..],
        &b"winhttp.dll"[..],
        &b"wininet.dll"[..],
        &b"httpapi.dll"[..],
    ] {
        assert!(
            !enthaelt(&klein, bibliothek),
            "{} bindet {} ein — FileTriage soll keinen Netzzugang haben",
            exe.display(),
            String::from_utf8_lossy(bibliothek)
        );
    }
}

/// Das Paketmanifest darf keine Netzwerk-Fähigkeit anfordern.
#[test]
fn das_paketmanifest_fordert_keinen_netzzugang() {
    let path = wurzel().parent().unwrap().join("packaging/msix/AppxManifest.template.xml");
    let Ok(manifest) = std::fs::read_to_string(path) else {
        // Der oeffentliche Community-Snapshot enthaelt kein MSIX/Store-Paket.
        return;
    };

    // Der Kommentarblock nennt die bewusst ausgelassenen Fähigkeiten. Er darf
    // die Prüfung nicht bestehen lassen, also wird er entfernt.
    let ohne_kommentare = manifest
        .split("<!--")
        .map(|t| t.split("-->").last().unwrap_or(""))
        .collect::<String>();

    for verboten in [
        "internetClient",
        "internetClientServer",
        "privateNetworkClientServer",
        "broadFileSystemAccess",
    ] {
        assert!(
            !ohne_kommentare.contains(verboten),
            "das Manifest fordert {verboten} an"
        );
    }
    assert!(
        ohne_kommentare.contains("runFullTrust"),
        "runFullTrust fehlt — ohne sie startet eine Win32-Anwendung im Paket nicht"
    );
}

/// Keine Quelltextkarten und keine Testdaten in der Oberfläche.
#[test]
fn die_oberflaeche_enthaelt_keine_entwicklungsreste() {
    let ui = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join("ui");
    for datei in walk(&ui) {
        let name = datei.file_name().unwrap().to_string_lossy().to_string();
        assert!(!name.ends_with(".map"), "Quelltextkarte ausgeliefert: {name}");
        let inhalt = std::fs::read_to_string(&datei).unwrap_or_default();
        assert!(
            !inhalt.contains("sourceMappingURL"),
            "{name} verweist auf eine Quelltextkarte"
        );
        assert!(!inhalt.contains("debugger;"), "{name} enthaelt einen Haltepunkt");
        // `mock.js` gehoert in den Vorschau-Harness, nicht ins Programm.
        assert!(name != "mock.js", "die Attrappe des Backends liegt im Programm");
    }
}

/// Das Release-Profil muss die Härtung enthalten, die wir behaupten.
#[test]
fn das_release_profil_ist_gehaertet() {
    let toml = lies("Cargo.toml");
    let profil = toml
        .split("[profile.release]")
        .nth(1)
        .expect("Release-Profil fehlt");
    for (schluessel, warum) in [
        ("lto = true", "ohne LTO bleiben unnoetige Symbole stehen"),
        ("codegen-units = 1", "mehr Einheiten heisst mehr Symbolnamen"),
        ("strip = true", "ohne strip bleiben Debug-Symbole im Programm"),
        ("panic = \"abort\"", "ohne abort bleibt der Abwickler samt Namen drin"),
    ] {
        assert!(profil.contains(schluessel), "{schluessel} fehlt — {warum}");
    }
}

// ---------------------------------------------------------------------------
// Hilfen
// ---------------------------------------------------------------------------

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(eintraege) = std::fs::read_dir(dir) else { return out };
    for e in eintraege.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}

fn enthaelt(heuhaufen: &[u8], nadel: &[u8]) -> bool {
    heuhaufen.windows(nadel.len()).any(|f| f == nadel)
}
