// Formatierung für die Anzeige.
//
// Alles, was von der Sprache abhängt, kommt aus `i18n.js` — Zahlen,
// Dateigrößen, Datum, relative Zeit, Kategorienamen, Tastennamen. Dieses
// Modul bleibt bestehen, damit die Ansichten ihre Aufrufe behalten, und
// reicht durch.
//
// Hier stand vorher deutsche Formatierung fest verdrahtet: `toFixed(1)` mit
// Komma, `toLocaleDateString('de-DE')`, „vor 3 Jahren". Das war richtig,
// solange es nur eine Sprache gab, und ist genau die Sorte Falle, die eine
// Übersetzung wertlos macht.

import {
  bytes as i18nBytes,
  datum as i18nDatum,
  datumZeit as i18nDatumZeit,
  relativeZeit,
  t,
} from './i18n.js';

/** Dateigröße in der Schreibweise der eingestellten Sprache. */
export const bytes = i18nBytes;

/** Unix-Sekunden als Datum. */
export const datum = i18nDatum;

/** Unix-Sekunden als Datum mit Uhrzeit. */
export const datumZeit = i18nDatumZeit;

/**
 * Grober Abstand zu heute, z. B. „vor 3 Jahren".
 *
 * `Intl.RelativeTimeFormat` kennt die Regeln jeder Sprache — auch die, bei
 * denen es für „gestern" ein eigenes Wort gibt (`numeric: 'auto'`).
 */
export function alter(ts) {
  return relativeZeit(ts);
}

/** Anzeigename einer Dateikategorie. */
export function kategorie(key) {
  if (!key) return '';
  const uebersetzt = t(`category.${key}`);
  // Fehlt der Schlüssel, liefert t() ihn selbst zurück — dann lieber den
  // rohen Wert zeigen als „category.unknown".
  return uebersetzt === `category.${key}` ? key : uebersetzt;
}

// Symbole kommen aus lib/icons.js. Emoji sind hier bewusst verschwunden: Sie
// sehen auf jedem System anders aus und lassen sich nicht einfärben.

/**
 * Kürzt lange Pfade in der Mitte, damit Anfang und Ende sichtbar bleiben.
 *
 * Pfade bleiben auch in Sprachen von rechts nach links von links nach rechts
 * lesbar — ein umgedrehter Pfad ist kein Pfad mehr. Die Oberfläche setzt dafür
 * `dir="ltr"` auf die Elemente, die Pfade zeigen.
 */
export function kurzerPfad(pfad, max = 64) {
  if (!pfad || pfad.length <= max) return pfad || '';
  const teile = pfad.split('\\');
  if (teile.length <= 2) return `…${pfad.slice(-max)}`;
  return `${teile[0]}\\…\\${teile.slice(-2).join('\\')}`;
}

/** Tastenname für die Anzeige. */
export function taste(k) {
  const namen = {
    ' ': 'key.space',
    Backspace: 'key.backspace',
    Enter: 'key.enter',
    ArrowRight: 'key.right',
    ArrowLeft: 'key.left',
  };
  if (namen[k]) return t(namen[k]);
  return k?.toUpperCase() ?? '';
}
