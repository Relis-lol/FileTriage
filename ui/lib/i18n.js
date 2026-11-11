// Sprachen.
//
// Regeln, die dieses Modul durchsetzt:
//
//   * Sichtbare Texte sollen aus `locales/<sprache>.json` kommen. Der
//     String-Audit ist heuristisch; 0 Fundstellen ist kein Vollstaendigkeitsbeweis.
//   * Keine zusammengesetzten Sätze. `"Found " + n + " files"` lässt sich
//     nicht übersetzen — im Deutschen steht die Zahl woanders, im Japanischen
//     das Zählwort dahinter, im Polnischen hängt die Endung an der Zahl.
//     Stattdessen vollständige Sätze mit Platzhaltern.
//   * Pluralformen über `Intl.PluralRules`. Englisch hat zwei Formen,
//     Polnisch vier, Arabisch sechs, Japanisch eine. Das kann man nicht mit
//     `n === 1 ? a : b` erschlagen.
//   * Zahlen, Daten und Dateigrößen über `Intl`. Nicht "1.234,5 MB" fest
//     verdrahten — das ist deutsche Schreibweise, sonst nichts.
//
// Kein Framework. `Intl` steckt in der WebView, und die Kataloge sind flache
// JSON-Dateien, die ein Übersetzer ohne Werkzeuge bearbeiten kann.

/**
 * Alle **mitgelieferten** Sprachen: Kennung -> Eigenname.
 *
 * Hier steht nur, wofuer es auch einen Katalog gibt. Eine Sprache anzubieten,
 * die dann zur Haelfte englisch erscheint, waere schlechter als sie
 * wegzulassen - `i18ncheck.rs` laesst das auch nicht zu.
 *
 * Die uebrigen 24 Sprachen aus der Produktplanung (Franzoesisch, Spanisch,
 * Italienisch, Niederlaendisch, Portugiesisch, Polnisch, Tschechisch,
 * Tuerkisch, Ukrainisch, Russisch, Japanisch, Koreanisch, Chinesisch,
 * Arabisch, Hindi, Indonesisch, Vietnamesisch, Thai, Schwedisch, Daenisch,
 * Norwegisch, Finnisch) sind vorbereitet, aber noch nicht uebersetzt:
 * `tools/make_locale.py` kennt ihre Pluralformen, und sobald ein Katalog
 * daliegt, gehoert die Sprache in diese Tabelle.
 */
export const SPRACHEN = {
  en: 'English',
  de: 'Deutsch',
};

/** Sprachen, die von rechts nach links gesetzt werden. */
export const RTL = new Set(['ar', 'he', 'fa', 'ur']);

/**
 * Entwickler-Pseudosprache.
 *
 * Sie ist kein Spaß, sondern ein Prüfmittel: Sie macht Text etwa 40 % länger
 * (so viel länger wird Deutsch gegenüber Englisch), verfremdet jeden
 * Buchstaben und setzt Klammern um jeden Satz. Was danach noch lesbar
 * dasteht, kommt nicht aus dem Katalog — und was aus der Fläche läuft, wird
 * es in einer echten Sprache auch tun.
 */
export const PSEUDO = 'pseudo';

const ERSATZ = {
  a: 'å', b: 'ƀ', c: 'ç', d: 'đ', e: 'ë', f: 'ƒ', g: 'ĝ', h: 'ĥ', i: 'ï',
  j: 'ĵ', k: 'ķ', l: 'ļ', m: 'ɱ', n: 'ñ', o: 'ø', p: 'þ', q: 'q', r: 'ŕ',
  s: 'š', t: 'ţ', u: 'ü', v: 'v', w: 'ŵ', x: 'ẋ', y: 'ý', z: 'ž',
  A: 'Å', B: 'Ɓ', C: 'Ç', D: 'Đ', E: 'Ë', F: 'Ƒ', G: 'Ĝ', H: 'Ĥ', I: 'Ï',
  J: 'Ĵ', K: 'Ķ', L: 'Ļ', M: 'Ṁ', N: 'Ñ', O: 'Ø', P: 'Þ', Q: 'Q', R: 'Ŕ',
  S: 'Š', T: 'Ţ', U: 'Ü', V: 'V', W: 'Ŵ', X: 'Ẋ', Y: 'Ý', Z: 'Ž',
};

function pseudoisiere(text) {
  // Platzhalter bleiben unangetastet, sonst findet sie niemand wieder.
  const teile = String(text).split(/(\{[^}]+\})/);
  const verfremdet = teile
    .map((t, i) => (i % 2 ? t : [...t].map((c) => ERSATZ[c] ?? c).join('')))
    .join('');
  // Verlängern, ohne Platzhalter zu verdoppeln.
  const fuellung = '·'.repeat(Math.max(2, Math.round(verfremdet.length * 0.4)));
  return `[!! ${verfremdet} ${fuellung} !!]`;
}

// ---------------------------------------------------------------------------
// Zustand
// ---------------------------------------------------------------------------

let katalog = {};
let rueckfall = {};
let aktuell = 'en';
let fehlend = new Set();

/** Die gerade verwendete Sprachkennung. */
export const sprache = () => aktuell;

/** BCP-47-Kennung für `Intl`. `zh-Hans` → `zh-Hans`, `pseudo` → `en`. */
export const locale = () => (aktuell === PSEUDO ? 'en' : aktuell);

export const istRtl = () => RTL.has(aktuell.split('-')[0]);

/** Schlüssel, die zur Laufzeit gefehlt haben. Für die Prüfung im Harness. */
export const fehlendeSchluessel = () => [...fehlend];

/**
 * Wählt aus dem, was Windows meldet, die beste mitgelieferte Sprache.
 *
 * `navigator.languages` gibt in der WebView die Anzeigesprachen des Systems
 * in der Reihenfolge zurück, die der Nutzer eingestellt hat. Gesucht wird
 * erst die genaue Kennung, dann die Sprache ohne Region.
 */
export function systemsprache(kandidaten = navigator.languages ?? [navigator.language]) {
  const vorhanden = Object.keys(SPRACHEN);
  const klein = new Map(vorhanden.map((k) => [k.toLowerCase(), k]));

  for (const roh of kandidaten) {
    if (!roh) continue;
    const k = String(roh).toLowerCase();
    if (klein.has(k)) return klein.get(k);

    // Chinesisch: Schrift entscheidet, nicht das Land.
    if (k.startsWith('zh')) {
      const traditionell = /hant|tw|hk|mo/.test(k);
      return traditionell ? 'zh-Hant' : 'zh-Hans';
    }
    // Portugiesisch: Brasilien und Portugal unterscheiden sich genug.
    if (k.startsWith('pt')) return k.includes('br') ? 'pt-BR' : 'pt-PT';
    if (k.startsWith('nb') || k.startsWith('no') || k.startsWith('nn')) return 'nb';

    const basis = k.split('-')[0];
    if (klein.has(basis)) return klein.get(basis);
  }
  return 'en';
}

async function ladeDatei(kennung) {
  const antwort = await fetch(`./locales/${kennung}.json`);
  if (!antwort.ok) throw new Error(`${kennung}: HTTP ${antwort.status}`);
  return antwort.json();
}

/**
 * Lädt eine Sprache. Englisch bleibt immer als Rückfallebene geladen: Ein
 * fehlender Schlüssel soll einen englischen Satz zeigen, nicht den Schlüssel.
 */
export async function ladeSprache(kennung) {
  if (!rueckfall.app_name) {
    rueckfall = await ladeDatei('en');
  }
  if (kennung === PSEUDO) {
    katalog = Object.fromEntries(
      Object.entries(rueckfall).map(([k, v]) => [
        k,
        typeof v === 'object' ? Object.fromEntries(Object.entries(v).map(([f, t]) => [f, pseudoisiere(t)])) : pseudoisiere(v),
      ])
    );
    aktuell = PSEUDO;
  } else if (kennung === 'en') {
    katalog = rueckfall;
    aktuell = 'en';
  } else {
    try {
      katalog = await ladeDatei(kennung);
      aktuell = kennung;
    } catch {
      // Eine fehlende Datei darf das Programm nicht anhalten.
      katalog = rueckfall;
      aktuell = 'en';
    }
  }
  fehlend = new Set();
  document.documentElement.lang = locale();
  document.documentElement.dir = istRtl() ? 'rtl' : 'ltr';
  return aktuell;
}

// ---------------------------------------------------------------------------
// Übersetzen
// ---------------------------------------------------------------------------

function einsetzen(vorlage, werte) {
  return String(vorlage).replace(/\{(\w+)\}/g, (ganz, name) => {
    if (!(name in werte)) return ganz;
    const w = werte[name];
    return typeof w === 'number' ? zahl(w) : String(w);
  });
}

/**
 * Übersetzt einen Schlüssel.
 *
 * Ein Eintrag ist entweder eine Zeichenkette oder — bei Mengenangaben — ein
 * Objekt mit den Pluralformen der jeweiligen Sprache (`one`, `other`, …).
 * Welche Form gilt, entscheidet `Intl.PluralRules` anhand von `params.count`.
 */
export function t(schluessel, params = {}) {
  let eintrag = katalog[schluessel];
  if (eintrag === undefined) {
    eintrag = rueckfall[schluessel];
    if (eintrag === undefined) {
      fehlend.add(schluessel);
      return schluessel;
    }
  }

  if (eintrag && typeof eintrag === 'object') {
    const n = Number(params.count ?? 0);
    const form = new Intl.PluralRules(locale()).select(n);
    // `other` ist in jeder Sprache vorhanden und damit die sichere Rückfallform.
    eintrag = eintrag[form] ?? eintrag.other ?? Object.values(eintrag)[0] ?? schluessel;
  }
  return einsetzen(eintrag, params);
}

// ---------------------------------------------------------------------------
// Zahlen, Größen, Zeit
// ---------------------------------------------------------------------------

export function zahl(n) {
  return new Intl.NumberFormat(locale()).format(n);
}

/**
 * Dateigröße.
 *
 * Bewusst dezimal (1 kB = 1000 Byte): So rechnet der Windows Explorer nicht,
 * aber so rechnen Festplattenhersteller und der Rest der Welt. Wichtiger ist,
 * dass Trennzeichen und Einheitenabstand aus der Sprache kommen — im
 * Französischen steht ein schmales Leerzeichen vor der Einheit, im Deutschen
 * ein normales.
 */
export function bytes(n) {
  const einheiten = ['byte', 'kilobyte', 'megabyte', 'gigabyte', 'terabyte'];
  let wert = Math.abs(Number(n) || 0);
  let i = 0;
  while (wert >= 1000 && i < einheiten.length - 1) {
    wert /= 1000;
    i += 1;
  }
  const stellen = i === 0 ? 0 : wert < 10 ? 1 : 0;
  try {
    return new Intl.NumberFormat(locale(), {
      style: 'unit',
      unit: einheiten[i],
      unitDisplay: 'short',
      maximumFractionDigits: stellen,
      minimumFractionDigits: stellen,
    }).format(wert);
  } catch {
    // Ältere WebViews kennen `style: 'unit'` nicht.
    const kurz = ['B', 'kB', 'MB', 'GB', 'TB'][i];
    return `${new Intl.NumberFormat(locale(), { maximumFractionDigits: stellen }).format(wert)} ${kurz}`;
  }
}

export function datum(ts) {
  if (!ts) return t('common.unknown');
  return new Intl.DateTimeFormat(locale(), { dateStyle: 'medium' }).format(new Date(ts * 1000));
}

export function datumZeit(ts) {
  if (!ts) return t('common.unknown');
  return new Intl.DateTimeFormat(locale(), {
    dateStyle: 'medium',
    timeStyle: 'short',
  }).format(new Date(ts * 1000));
}

/** „vor 3 Tagen“ — über `Intl.RelativeTimeFormat`, nicht selbst gebaut. */
export function relativeZeit(ts) {
  if (!ts) return t('common.unknown');
  const sekunden = ts - Date.now() / 1000;
  const stufen = [
    ['year', 31536000],
    ['month', 2592000],
    ['day', 86400],
    ['hour', 3600],
    ['minute', 60],
  ];
  const rtf = new Intl.RelativeTimeFormat(locale(), { numeric: 'auto' });
  for (const [einheit, laenge] of stufen) {
    if (Math.abs(sekunden) >= laenge) {
      return rtf.format(Math.round(sekunden / laenge), einheit);
    }
  }
  return rtf.format(Math.round(sekunden), 'second');
}
