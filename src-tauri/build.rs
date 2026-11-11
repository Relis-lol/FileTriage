//! Bauzeit-Schritte.
//!
//! Der Vertriebskanal wird hier eingebrannt, nicht zur Laufzeit gelesen. Ein
//! Schalter, der sich nach dem Bauen noch umlegen laesst, waere genau die
//! Hintertuer, die es nicht geben soll.

fn main() {
    // Ohne Angabe gilt "community": Der oeffentliche Quellstand baut damit
    // reproduzierbar die zeitlich unbegrenzte, nichtkommerzielle Ausgabe.
    let kanal = std::env::var("FILETRIAGE_CHANNEL").unwrap_or_else(|_| "community".into());
    assert!(
        matches!(kanal.as_str(), "community" | "direct" | "store"),
        "ungueltiger FILETRIAGE_CHANNEL: {kanal}"
    );
    println!("cargo:rerun-if-env-changed=FILETRIAGE_CHANNEL");
    println!("cargo:rustc-env=FILETRIAGE_CHANNEL={kanal}");

    tauri_build::build();
}
