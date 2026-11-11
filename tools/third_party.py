"""Erzeugt THIRD_PARTY_NOTICES.md aus dem tatsaechlichen Abhaengigkeitsbaum.

Nicht aus einer gepflegten Liste - die veraltet. `cargo tree` sagt, was
wirklich mitgebaut wird.

Zusaetzlich meldet das Werkzeug Lizenzen, die bei geschlossener,
kommerzieller Weitergabe Pflichten ausloesen. Das ist keine Rechtsberatung;
es ist eine Liste der Stellen, an denen ein Anwalt hinsehen muss.

Aufruf:

    python tools/third_party.py            schreibt THIRD_PARTY_NOTICES.md
    python tools/third_party.py --check    meldet nur Auffaelligkeiten
"""

from __future__ import annotations

import argparse
import io
import os
import re
import subprocess
import sys
from collections import defaultdict

WURZEL = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SRC = os.path.join(WURZEL, 'src-tauri')

# Lizenzen, die eine Weitergabe in geschlossener Form ohne Weiteres erlauben.
# Pflicht bleibt in allen Faellen die Nennung von Lizenztext und Copyright.
FREIZUEGIG = {
    'MIT', 'MIT-0', 'Apache-2.0', 'BSD-2-Clause', 'BSD-3-Clause', '0BSD',
    'ISC', 'Zlib', 'Unlicense', 'CC0-1.0', 'Unicode-3.0', 'BSL-1.0',
    'Apache-2.0 WITH LLVM-exception', 'NCSA',
}

# Lizenzen, die eigene Pflichten ausloesen - hier muss jemand hinsehen.
AUFMERKSAMKEIT = {
    'MPL-2.0': (
        'Dateiweises Copyleft. Statisches Einbinden in ein geschlossenes '
        'Programm ist erlaubt, aber der Quelltext der MPL-Dateien muss den '
        'Empfaengern zugaenglich sein und die Lizenz muss beiliegen.'
    ),
    'LGPL-2.1': ('Erfordert Austauschbarkeit der Bibliothek. Bei statischem '
                 'Linken in ein geschlossenes Programm problematisch.'),
    'LGPL-3.0': ('Erfordert Austauschbarkeit der Bibliothek. Bei statischem '
                 'Linken in ein geschlossenes Programm problematisch.'),
    'GPL-2.0': 'Starkes Copyleft. Mit geschlossener Weitergabe unvereinbar.',
    'GPL-3.0': 'Starkes Copyleft. Mit geschlossener Weitergabe unvereinbar.',
    'AGPL-3.0': 'Starkes Copyleft. Mit geschlossener Weitergabe unvereinbar.',
}

MPL_SOURCES = {
    'cssparser': 'https://crates.io/crates/cssparser/0.36.0',
    'cssparser-macros': 'https://crates.io/crates/cssparser-macros/0.6.1',
    'dtoa-short': 'https://crates.io/crates/dtoa-short/0.3.5',
    'option-ext': 'https://crates.io/crates/option-ext/0.2.0',
    'selectors': 'https://crates.io/crates/selectors/0.36.1',
}


def baum() -> list[tuple[str, str, str, str]]:
    """(Name, Version, Lizenz, Quelle)"""
    roh = subprocess.run(
        ['cargo', 'tree', '--prefix', 'none', '--format', '{p}|{l}|{r}'],
        cwd=SRC, capture_output=True, text=True, encoding='utf-8', errors='replace',
    )
    if roh.returncode != 0:
        raise SystemExit('cargo tree fehlgeschlagen:\n' + roh.stderr[-2000:])

    gesehen = {}
    for zeile in roh.stdout.splitlines():
        zeile = zeile.strip().removesuffix(' (*)')
        if not zeile or '|' not in zeile:
            continue
        teile = zeile.split('|')
        paket = teile[0].strip()
        lizenz = (teile[1] if len(teile) > 1 else '').strip()
        quelle = (teile[2] if len(teile) > 2 else '').strip()
        m = re.match(r'^(\S+)\s+v(\S+)', paket)
        if not m:
            continue
        name, version = m.group(1), m.group(2)
        if name == 'filetriage':
            continue
        gesehen[(name, version)] = (lizenz or 'nicht angegeben', quelle)
    return sorted((n, v, l, q) for (n, v), (l, q) in gesehen.items())


def einzelteile(ausdruck: str) -> set[str]:
    """Zerlegt einen SPDX-Ausdruck grob in die genannten Lizenzen."""
    roh = re.split(r'\s+(?:OR|AND)\s+|/', ausdruck)
    return {t.strip(' ()') for t in roh if t.strip(' ()')}


def bewerte(ausdruck: str) -> tuple[str, str]:
    """(Einstufung, Begruendung)"""
    teile = einzelteile(ausdruck)
    # Bei einem OR reicht eine freizuegige Option: Man darf sie waehlen.
    if ' OR ' in ausdruck or '/' in ausdruck:
        if teile & FREIZUEGIG:
            return 'ok', ''
    heikel = teile & set(AUFMERKSAMKEIT)
    if heikel:
        name = sorted(heikel)[0]
        return 'pruefen', AUFMERKSAMKEIT[name]
    if teile <= FREIZUEGIG:
        return 'ok', ''
    return 'unklar', 'Lizenz nicht in der bekannten Liste.'


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--check', action='store_true')
    args = p.parse_args()

    pakete = baum()
    nach_lizenz = defaultdict(list)
    auffaellig = []
    for name, version, lizenz, quelle in pakete:
        nach_lizenz[lizenz].append((name, version, quelle))
        einstufung, grund = bewerte(lizenz)
        if einstufung != 'ok':
            auffaellig.append((name, version, lizenz, einstufung, grund))

    if args.check:
        for name, version, lizenz, einstufung, grund in auffaellig:
            print(f'[{einstufung}] {name} {version} — {lizenz}\n        {grund}')
        print(f'\n{len(pakete)} Pakete, {len(auffaellig)} zu pruefen')
        blockers = [item for item in auffaellig if item[2] != 'MPL-2.0']
        mpl_ok = os.path.isfile(os.path.join(WURZEL, 'LICENSES', 'MPL-2.0.txt'))
        if any(item[2] == 'MPL-2.0' for item in auffaellig) and not mpl_ok:
            print('\n[blocker] LICENSES/MPL-2.0.txt fehlt')
            blockers.append(('MPL-2.0', '', '', '', ''))
        return 1 if blockers else 0

    zeilen = []
    a = zeilen.append
    a('# Third-party notices')
    a('')
    a('FileTriage uses the packages listed below. This file is generated from')
    a('the actual locked dependency tree (`python tools/third_party.py`) rather')
    a('than from a manually maintained package list.')
    a('')
    a(f'Inventory: {len(pakete)} packages.')
    a('')
    a('> This inventory is generated from the locked dependency tree. It is not')
    a('> legal advice and does not relicense any third-party component.')
    a('')

    a('## MPL 2.0 components')
    a('')
    a('The following source packages remain licensed under MPL 2.0. Their exact')
    a('source is available from crates.io, and the complete license text is in')
    a('[`LICENSES/MPL-2.0.txt`](LICENSES/MPL-2.0.txt).')
    a('')
    a('| Package | Version | Source |')
    a('|---|---:|---|')
    for name, version, lizenz, _e, _grund in auffaellig:
        if lizenz == 'MPL-2.0':
            a(f'| `{name}` | {version} | <{MPL_SOURCES[name]}> |')
    a('')

    a('## Licenses with additional obligations')
    a('')
    if not auffaellig:
        a('None.')
    else:
        a('These packages retain license obligations that apply independently of')
        a('the FileTriage license:')
        a('')
        a('| Package | Version | License | Obligation |')
        a('|---|---|---|---|')
        for name, version, lizenz, _e, grund in auffaellig:
            a(f'| `{name}` | {version} | {lizenz} | {grund} |')
    a('')

    a('## All packages by license')
    a('')
    for lizenz in sorted(nach_lizenz):
        eintraege = sorted(nach_lizenz[lizenz])
        a(f'### {lizenz}')
        a('')
        a(f'{len(eintraege)} packages:')
        a('')
        a(', '.join(f'`{n} {v}`' for n, v, _q in eintraege))
        a('')

    a('## Sources')
    a('')
    a('All packages come from <https://crates.io> unless the list says otherwise.')
    a('Package source is available at')
    a('`https://crates.io/crates/<name>/<version>` and each package includes its')
    a('own license information.')
    a('')
    a('The WebView component is supplied by the operating system (Microsoft Edge')
    a('WebView2) and is not bundled as a Rust package.')
    a('')

    ziel = os.path.join(WURZEL, 'THIRD_PARTY_NOTICES.md')
    io.open(ziel, 'w', encoding='utf-8', newline='\n').write('\n'.join(zeilen))
    print(f'{len(pakete)} Pakete, {len(auffaellig)} zu pruefen -> {ziel}')
    return 0


if __name__ == '__main__':
    sys.exit(main())
