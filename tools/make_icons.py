"""Erzeugt das FileTriage-App-Icon.

Motiv: ein Posteingang mit Haken. Der Posteingang steht fuer "Inbox Zero",
der Haken fuer die getroffene Entscheidung. Beides sind einfache geometrische
Formen, damit sie bei 16x16 nicht zu Matsch werden.

Gestaltungsregeln:

  * ein abgerundetes Quadrat als Traeger, damit das Icon auf hellen und
    dunklen Taskleisten gleich gut sitzt
  * eine Farbe plus Weiss - kein Verlauf, keine Schatten
  * dicke Formen, grosse Abstaende: alles duenner als 1/12 der Kantenlaenge
    verschwindet bei kleinen Groessen
  * bei 16 und 20 Pixeln bleibt nur der Haken. Fach und Haken zusammen werden
    dort beide unkenntlich; ein Haken allein sagt weiterhin "erledigt".

Aufruf:  python tools/make_icons.py
"""

import os

from PIL import Image, ImageDraw

OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "src-tauri", "icons")

# Blau wie der Akzent der Oberflaeche, damit Programm und Symbol zusammenpassen.
BLAU = (37, 99, 235, 255)
WEISS = (255, 255, 255, 255)

# Intern wird 8-fach vergroessert gezeichnet und dann herunterskaliert -
# das ergibt saubere Kanten ohne eigene Kantenglaettung.
SS = 8


def zeichne(size: int) -> Image.Image:
    """Zeichnet das Icon in der gewuenschten Kantenlaenge."""
    gross = size * SS
    bild = Image.new("RGBA", (gross, gross), (0, 0, 0, 0))
    d = ImageDraw.Draw(bild)

    # --- Traeger: abgerundetes Quadrat ------------------------------------
    rand = gross * 0.055
    radius = gross * 0.22
    d.rounded_rectangle(
        [rand, rand, gross - rand, gross - rand],
        radius=radius,
        fill=BLAU,
    )

    # Bewusst einfarbig: Ein zweiter Blauton erzeugt bei kleinen Groessen nur
    # eine sichtbare Naht und keine Tiefe.

    klein = size <= 20
    if klein:
        # Unter 24 Pixeln bleibt nur der Haken. Ablagefach plus Haken werden
        # dort beide unkenntlich, und ein Haken allein sagt immer noch
        # "erledigt" - genau darum geht es.
        strich = gross * 0.155
        punkte = [
            (gross * 0.285, gross * 0.505),
            (gross * 0.435, gross * 0.655),
            (gross * 0.725, gross * 0.345),
        ]
        d.line([punkte[0], punkte[1]], fill=WEISS, width=int(strich))
        d.line([punkte[1], punkte[2]], fill=WEISS, width=int(strich))
        for pt in punkte:
            r = strich / 2
            d.ellipse([pt[0] - r, pt[1] - r, pt[0] + r, pt[1] + r], fill=WEISS)
        return bild.resize((size, size), Image.LANCZOS)

    # --- Ablagefach --------------------------------------------------------
    # Ein Trapez mit Ausschnitt in der Mitte - die klassische Postfach-Form.
    links = gross * 0.20
    rechts = gross * 0.80
    oben = gross * 0.545
    unten = gross * 0.775
    dicke = gross * 0.075

    # Wanne
    d.polygon(
        [
            (links, oben),
            (rechts, oben),
            (rechts - dicke * 0.6, unten),
            (links + dicke * 0.6, unten),
        ],
        fill=WEISS,
    )
    # Ausschnitt, durch den die Datei faellt
    schlitz_b = gross * 0.155
    d.polygon(
        [
            (gross * 0.5 - schlitz_b, oben - gross * 0.004),
            (gross * 0.5 + schlitz_b, oben - gross * 0.004),
            (gross * 0.5 + schlitz_b * 0.86, oben + gross * 0.085),
            (gross * 0.5 - schlitz_b * 0.86, oben + gross * 0.085),
        ],
        fill=BLAU,
    )

    # --- Haken -------------------------------------------------------------
    # Zwei dicke Linien mit runden Enden, deutlich ueber dem Fach.
    strich = gross * 0.105
    punkte = [
        (gross * 0.345, gross * 0.325),
        (gross * 0.455, gross * 0.435),
        (gross * 0.675, gross * 0.215),
    ]
    d.line([punkte[0], punkte[1]], fill=WEISS, width=int(strich))
    d.line([punkte[1], punkte[2]], fill=WEISS, width=int(strich))
    for p in punkte:
        r = strich / 2
        d.ellipse([p[0] - r, p[1] - r, p[0] + r, p[1] + r], fill=WEISS)

    return bild.resize((size, size), Image.LANCZOS)


def schreibe_ico(pfad: str, groessen) -> None:
    """ICO mit allen von Windows verlangten Groessen."""
    bilder = [zeichne(g) for g in groessen]
    # Pillow schreibt die uebrigen Groessen selbst, wenn man sie angibt.
    bilder[-1].save(pfad, format="ICO", sizes=[(g, g) for g in groessen])


# Ziel fuer die Paketbilder des Microsoft Stores.
MSIX = os.path.join(
    os.path.dirname(os.path.abspath(__file__)), "..", "packaging", "msix", "Assets"
)


def zeichne_breit(breite: int, hoehe: int) -> Image.Image:
    """Breite Kachel: dasselbe Motiv, mittig auf durchsichtigem Grund.

    Der Traeger bleibt quadratisch. Ein in die Breite gezogenes Icon sieht
    immer nach Versehen aus.
    """
    kante = min(breite, hoehe)
    bild = Image.new("RGBA", (breite, hoehe), (0, 0, 0, 0))
    motiv = zeichne(kante)
    bild.paste(motiv, ((breite - kante) // 2, (hoehe - kante) // 2), motiv)
    return bild


def schreibe_msix() -> None:
    """Die Bilder, die ein MSIX-Paket verlangt.

    Die Namen sind vorgeschrieben - Windows sucht sie genau so. Die
    Massstabsvarianten (scale-100 bis scale-400) sind kein Luxus: Ohne sie
    skaliert Windows selbst, und das Ergebnis ist auf hochaufloesenden
    Bildschirmen sichtbar unscharf.
    """
    ziel = os.path.normpath(MSIX)
    os.makedirs(ziel, exist_ok=True)

    # Grundgroessen mit ihren Massstaeben.
    quadrate = {
        "Square44x44Logo": 44,
        "Square71x71Logo": 71,
        "Square150x150Logo": 150,
        "Square310x310Logo": 310,
        "StoreLogo": 50,
    }
    massstaebe = [100, 125, 150, 200, 400]
    geschrieben = 0
    for name, basis in quadrate.items():
        zeichne(basis).save(os.path.join(ziel, f"{name}.png"), format="PNG")
        geschrieben += 1
        for m in massstaebe:
            groesse = round(basis * m / 100)
            zeichne(groesse).save(
                os.path.join(ziel, f"{name}.scale-{m}.png"), format="PNG"
            )
            geschrieben += 1

    # Kleine Zielgroessen fuer Taskleiste und Explorer. "unplated" heisst:
    # ohne den farbigen Kasten, den Windows sonst dahinterlegt.
    for groesse in [16, 24, 32, 48, 256]:
        bild = zeichne(groesse)
        bild.save(
            os.path.join(ziel, f"Square44x44Logo.targetsize-{groesse}.png"), format="PNG"
        )
        bild.save(
            os.path.join(
                ziel, f"Square44x44Logo.targetsize-{groesse}_altform-unplated.png"
            ),
            format="PNG",
        )
        geschrieben += 2

    for m in [100, 125, 150, 200, 400]:
        zeichne_breit(round(310 * m / 100), round(150 * m / 100)).save(
            os.path.join(ziel, f"Wide310x150Logo.scale-{m}.png"), format="PNG"
        )
        geschrieben += 1
    zeichne_breit(310, 150).save(
        os.path.join(ziel, "Wide310x150Logo.png"), format="PNG"
    )
    geschrieben += 1

    print(f"{geschrieben} Paketbilder geschrieben nach {ziel}")


def main() -> None:
    ziel = os.path.normpath(OUT)
    os.makedirs(ziel, exist_ok=True)

    # PNGs fuer den Tauri-Bundler und die Oberflaeche.
    for groesse, name in [
        (32, "32x32.png"),
        (128, "128x128.png"),
        (256, "128x128@2x.png"),
        (512, "icon.png"),
    ]:
        zeichne(groesse).save(os.path.join(ziel, name), format="PNG")

    # ICO fuer EXE, Taskleiste, Startmenue, Installer und Verknuepfungen.
    schreibe_ico(os.path.join(ziel, "icon.ico"), [16, 20, 24, 32, 48, 64, 128, 256])

    print("Icons geschrieben nach", ziel)
    schreibe_msix()


if __name__ == "__main__":
    main()
