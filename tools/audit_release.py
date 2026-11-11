"""Durchsucht die fertigen Release-Dateien nach Dingen, die dort nicht hingehoeren.

Ein Release-Artefakt ist das einzige, was den Rechner des Entwicklers verlaesst.
Alles, was darin steckt, ist veroeffentlicht - auch das, was niemand
veroeffentlichen wollte. Dieses Werkzeug sucht deshalb nach:

  * Pfaden aus dem Benutzerprofil des Entwicklers
  * Verweisen auf die Quelldateien und die Crate-Registry
  * E-Mail-Adressen, Tokens, Schluesseln
  * localhost und Netzwerkadressen
  * Source Maps und Debug-Dateien

Aufruf:

    python tools/audit_release.py
    python tools/audit_release.py --json      (fuer Automatisierung)

Rueckgabewert 0, wenn nichts gefunden wurde, sonst 1.

Das ersetzt keine Pruefung durch einen Menschen. Es verhindert aber, dass eine
der Fundstellen still wieder auftaucht.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys

WURZEL = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# Dateien, die tatsaechlich ausgeliefert werden.
KANDIDATEN = [
    os.path.join("src-tauri", "target", "release", "filetriage.exe"),
    os.path.join("release", "FileTriage_0.1.0-rc4-store_x64-setup.exe"),
    os.path.join("release", "FileTriage_0.1.0-rc3_x64-setup.exe"),
    os.path.join("release", "FileTriage.msix"),
]

BS = chr(92)


def muster() -> list[tuple[str, bytes, str]]:
    """(Name, Regex, Schweregrad)."""
    u = BS + BS  # ein Backslash im Regex
    return [
        (
            "Benutzerprofil-Pfad",
            (r"C:" + u + r"Users" + u + r"[A-Za-z][A-Za-z0-9_~.-]{2,20}" + u).encode(),
            "hoch",
        ),
        # Der fuehrende Punkt muss auf einem Trenner sitzen. Ohne diese
        # Verankerung schlug das Muster auf einem zufaelligen 0x2E-Byte im
        # Binaerteil an und machte den Bericht unglaubwuerdig.
        (
            "Crate-Registry",
            (r"[\\\\/]\.cargo" + u + r"registry").encode(),
            "hoch",
        ),
        ("Quelldateipfad", (r"src-tauri" + u + r"src" + u).encode(), "mittel"),
        # Bewusst eng: Die naheliegende Fassung fand in einer Binaerdatei
        # zuverlaessig Unsinn wie "128x128@2x.png" und machte den Bericht
        # wertlos. Verlangt werden ein plausibler lokaler Teil und eine
        # bekannte Endung.
        (
            "E-Mail-Adresse",
            rb"[A-Za-z0-9._%+-]{3,}@[A-Za-z0-9-]{3,}\.(?:com|net|org|de|io|dev|me|eu|co|uk|at|ch)",
            "hoch",
        ),
        ("GitHub-Token", rb"gh[pousr]_[A-Za-z0-9]{20,}", "hoch"),
        ("AWS-Schluessel", rb"AKIA[0-9A-Z]{16}", "hoch"),
        ("Privater Schluessel", rb"BEGIN [A-Z ]*PRIVATE KEY", "hoch"),
        ("localhost", rb"(?i)https?://localhost", "mittel"),
        ("Source Map", rb"sourceMappingURL", "mittel"),
        ("Testordner", rb"filetriage-tests", "mittel"),
        ("Scratchpad", rb"(?i)scratchpad", "mittel"),
    ]


# Bekannte, harmlose Treffer. Jede Ausnahme braucht eine Begruendung.
AUSNAHMEN = {
    # Tauri spricht ueber diese Adresse mit dem eigenen Fenster. Sie geht nie
    # ins Netz - das Schema wird prozessintern abgefangen.
    "localhost": [b"http://ipc.localhost", b"https://tauri.localhost"],
    # Die Fehlermeldung des Papierkorbs nennt den Windows-Ordner, nicht den
    # Entwickler.
    "Benutzerprofil-Pfad": [],
}


def pruefe(pfad: str) -> list[dict]:
    with open(pfad, "rb") as f:
        daten = f.read()
    funde = []
    for name, regex, schwere in muster():
        treffer = sorted({t for t in re.findall(regex, daten)})
        erlaubt = AUSNAHMEN.get(name, [])
        treffer = [t for t in treffer if not any(a in t for a in erlaubt)]
        if treffer:
            funde.append(
                {
                    "muster": name,
                    "schwere": schwere,
                    "anzahl": len(treffer),
                    "beispiele": [t.decode("latin1") for t in treffer[:5]],
                }
            )
    return funde


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--json", action="store_true", help="Ergebnis als JSON ausgeben")
    p.add_argument("datei", nargs="*", help="zusaetzliche Dateien")
    args = p.parse_args()

    dateien = [os.path.join(WURZEL, k) for k in KANDIDATEN] + args.datei
    dateien = [d for d in dateien if os.path.isfile(d)]
    if not dateien:
        print("Keine Release-Datei gefunden. Erst bauen.")
        return 1

    bericht = {}
    schlecht = False
    for d in dateien:
        funde = pruefe(d)
        bericht[os.path.relpath(d, WURZEL)] = funde
        if funde:
            schlecht = True

    if args.json:
        print(json.dumps(bericht, indent=2, ensure_ascii=False))
        return 1 if schlecht else 0

    for datei, funde in bericht.items():
        groesse = os.path.getsize(os.path.join(WURZEL, datei))
        print(f"\n{datei}  ({groesse / 1024 / 1024:.2f} MB)")
        if not funde:
            print("  sauber")
            continue
        for f in funde:
            print(f"  [{f['schwere']}] {f['muster']}: {f['anzahl']}x")
            for b in f["beispiele"]:
                print(f"      {b}")
    print()
    return 1 if schlecht else 0


if __name__ == "__main__":
    sys.exit(main())
