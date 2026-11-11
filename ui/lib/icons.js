// Icon-System.
//
// Ein einheitlicher Satz Strichsymbole, lokal im Programm, ohne CDN und ohne
// Paketabhängigkeit. Alle Symbole teilen dieselben Regeln, damit sie wie ein
// Satz aussehen und nicht wie eine Sammlung:
//
//   * 24×24-Raster
//   * Strichstärke 1.75, keine Flächen
//   * runde Enden und Ecken
//   * gleiche optische Gewichtung
//
// Sie sind für dieses Projekt gezeichnet, in der Formensprache verbreiteter
// Strich-Sets wie Lucide. Damit hängt nichts von einem fremden Paket ab und es
// gibt keine Lizenzfrage. Wer später auf ein fertiges Set wechselt, tauscht
// diese eine Datei aus — der Rest der Oberfläche ruft nur `icon(name)` auf.
//
// Emoji werden bewusst nicht als Bedienelemente verwendet: Sie sehen auf jedem
// System anders aus, lassen sich nicht einfärben und wirken nicht wie ein
// Programm, dem man seine Dateien anvertraut.

const NS = 'http://www.w3.org/2000/svg';

/** Pfad-Definitionen. Werte sind Listen von SVG-Kindelementen. */
const FORMEN = {
  // --- Navigation ---------------------------------------------------------
  home: ['M4 10.5 12 4l8 6.5', 'M6 9.5V19a1 1 0 0 0 1 1h10a1 1 0 0 0 1-1V9.5'],
  inbox: [
    'M21 13h-5l-1.5 2.5h-5L8 13H3',
    'M6.2 4.8 3 13v5a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-5l-3.2-8.2A2 2 0 0 0 15.9 3.5H8.1a2 2 0 0 0-1.9 1.3z',
  ],
  copy: [
    { rect: [9, 9, 12, 12, 2] },
    'M5 15H4.5A1.5 1.5 0 0 1 3 13.5v-9A1.5 1.5 0 0 1 4.5 3h9A1.5 1.5 0 0 1 15 4.5V5',
  ],
  drive: [
    { rect: [2.5, 13.5, 19, 6.5, 2] },
    'M5.5 13.5 8 4.5h8l2.5 9',
    { circle: [7, 16.8, 1] },
  ],
  clock: [{ circle: [12, 12, 8.5] }, 'M12 7v5.2l3.2 1.9'],
  rules: ['M4 7h9', 'M4 12h9', 'M4 17h9', 'M16.5 6.5 18 8l3-3', 'M16.5 16.5 18 18l3-3'],
  search: [{ circle: [11, 11, 6.5] }, 'M16 16l4.5 4.5'],
  settings: [
    'M4 7h3.5',
    'M11.5 7H20',
    'M4 12h8.5',
    'M16.5 12H20',
    'M4 17h5.5',
    'M13.5 17H20',
    { circle: [9.5, 7, 2] },
    { circle: [14.5, 12, 2] },
    { circle: [11.5, 17, 2] },
  ],
  shieldAlert: [
    'M12 3l7.5 2.8v5.7c0 4.7-3.2 8.8-7.5 10.2-4.3-1.4-7.5-5.5-7.5-10.2V5.8z',
    'M12 8.5v4',
    { circle: [12, 16, 0.6] },
  ],
  history: ['M3.5 12a8.5 8.5 0 1 0 2.5-6', 'M3.5 4v4.5H8', 'M12 8.5V12l2.8 1.7'],

  // --- Ordner und Dateien -------------------------------------------------
  folder: ['M3.5 7.5a2 2 0 0 1 2-2H9l2 2h7.5a2 2 0 0 1 2 2v7.5a2 2 0 0 1-2 2h-13a2 2 0 0 1-2-2z'],
  folderOpen: [
    'M3.5 18.5V7.5a2 2 0 0 1 2-2H9l2 2h7.5a2 2 0 0 1 2 2v1',
    'M3.5 18.5l2.2-6.2a1.6 1.6 0 0 1 1.5-1.1h13.3l-2.2 6.2a1.6 1.6 0 0 1-1.5 1.1z',
  ],
  folderPlus: [
    'M3.5 7.5a2 2 0 0 1 2-2H9l2 2h7.5a2 2 0 0 1 2 2v7.5a2 2 0 0 1-2 2h-13a2 2 0 0 1-2-2z',
    'M12 10.5v5',
    'M9.5 13h5',
  ],
  file: ['M14 3.5v4.5a1 1 0 0 0 1 1h4.5', 'M14 3.5H7a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8.5z'],
  fileText: [
    'M14 3.5v4.5a1 1 0 0 0 1 1h4.5',
    'M14 3.5H7a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8.5z',
    'M8.5 13h7',
    'M8.5 16.5h4.5',
  ],
  fileCode: [
    'M14 3.5v4.5a1 1 0 0 0 1 1h4.5',
    'M14 3.5H7a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8.5z',
    'M10 13l-1.5 1.75L10 16.5',
    'M14 13l1.5 1.75L14 16.5',
  ],
  image: [{ rect: [3.5, 4.5, 17, 15, 2] }, { circle: [8.75, 9.75, 1.5] }, 'M20.5 15l-4.5-4.5-8 9'],
  video: [{ rect: [3, 5, 18, 14, 2] }, 'M10 9.5l5 2.5-5 2.5z'],
  music: ['M9.5 17.5V5.5l10-1.8v11.6', { circle: [7, 17.5, 2.5] }, { circle: [17, 15.6, 2.5] }],
  archive: [{ rect: [3.5, 8.5, 17, 11, 1.5] }, { rect: [2.5, 4, 19, 4.5, 1.5] }, 'M10 12.5h4'],
  package: ['M12 3l8.5 4.5v9L12 21l-8.5-4.5v-9z', 'M3.5 7.5 12 12l8.5-4.5', 'M12 12v9'],
  disc: [{ circle: [12, 12, 8.5] }, { circle: [12, 12, 2.4] }],
  book: [
    'M4.5 18.5A2 2 0 0 1 6.5 16.5H19.5',
    'M6.5 3.5h13v17h-13a2 2 0 0 1-2-2v-13a2 2 0 0 1 2-2z',
  ],
  font: ['M5 7.5V5.5h14v2', 'M12 5.5v13', 'M9 18.5h6'],
  link: [
    'M10.5 13.5a4 4 0 0 0 6 .4l2.4-2.4a4 4 0 0 0-5.6-5.6l-1.4 1.3',
    'M13.5 10.5a4 4 0 0 0-6-.4l-2.4 2.4a4 4 0 0 0 5.6 5.6l1.4-1.3',
  ],
  hourglass: ['M7 3.5h10', 'M7 20.5h10', 'M8 3.5v3.2L12 11l4-4.3V3.5', 'M8 20.5v-3.2L12 13l4 4.3v3.2'],
  transfer: ['M8 4v13', 'M4.5 13.5 8 17l3.5-3.5', 'M16 20V7', 'M12.5 10.5 16 7l3.5 3.5'],
  monitor: [{ rect: [2.5, 4.5, 19, 12.5, 2] }, 'M9 20.5h6', 'M12 17v3.5'],
  download: ['M12 4v10.5', 'M8 11l4 4 4-4', 'M4.5 19.5h15'],

  // --- Aktionen -----------------------------------------------------------
  play: ['M8 5.5 19 12 8 18.5z'],
  pause: ['M9.5 5.5v13', 'M14.5 5.5v13'],
  trash: [
    'M4 7h16',
    'M9.5 11v6',
    'M14.5 11v6',
    'M6.5 7l.8 11.6a2 2 0 0 0 2 1.9h5.4a2 2 0 0 0 2-1.9L17.5 7',
    'M9.5 7V5.4a1.4 1.4 0 0 1 1.4-1.4h2.2a1.4 1.4 0 0 1 1.4 1.4V7',
  ],
  skip: ['M7 6l6 6-6 6', 'M14 6l6 6-6 6'],
  undo: ['M3.5 12a8.5 8.5 0 1 0 2.5-6', 'M3.5 4v4.5H8'],
  redo: ['M20.5 12a8.5 8.5 0 1 1-2.5-6', 'M20.5 4v4.5H16'],
  externalLink: ['M14.5 4h5.5v5.5', 'M10.5 13.5 20 4', 'M19 13.5v5a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2v-11a2 2 0 0 1 2-2h5'],
  check: ['M5 12.5 9.5 17 19 7'],
  checkCircle: [{ circle: [12, 12, 8.5] }, 'M8.5 12.2l2.4 2.4 4.6-4.8'],
  x: ['M6.5 6.5l11 11', 'M17.5 6.5l-11 11'],
  xCircle: [{ circle: [12, 12, 8.5] }, 'M9.5 9.5l5 5', 'M14.5 9.5l-5 5'],
  plus: ['M12 5.5v13', 'M5.5 12h13'],
  minus: ['M5.5 12h13'],
  pencil: ['M4 20h4.2L20 8.2a2.3 2.3 0 0 0-3.2-3.2L5 16.8z', 'M15.5 5.8 18.2 8.5'],
  eye: ['M2.5 12S6 6 12 6s9.5 6 9.5 6-3.5 6-9.5 6-9.5-6-9.5-6z', { circle: [12, 12, 2.8] }],
  refresh: ['M20.5 12a8.5 8.5 0 1 1-2.5-6', 'M20.5 3.5V8H16'],
  alertTriangle: [
    'M12 4.2l8.4 14.6a1 1 0 0 1-.9 1.5H4.5a1 1 0 0 1-.9-1.5z',
    'M12 10v3.8',
    { circle: [12, 17, 0.6] },
  ],
  info: [{ circle: [12, 12, 8.5] }, 'M12 11.5v4.5', { circle: [12, 8.5, 0.6] }],
  shield: ['M12 3l7.5 2.8v5.7c0 4.7-3.2 8.8-7.5 10.2-4.3-1.4-7.5-5.5-7.5-10.2V5.8z'],
  chevronRight: ['M9.5 5.5 16 12l-6.5 6.5'],
  chevronLeft: ['M14.5 5.5 8 12l6.5 6.5'],
  chevronDown: ['M5.5 9.5 12 16l6.5-6.5'],
  arrowRight: ['M4.5 12h14', 'M13 6.5 18.5 12 13 17.5'],
  moreHorizontal: [{ circle: [5.5, 12, 1.2] }, { circle: [12, 12, 1.2] }, { circle: [18.5, 12, 1.2] }],
  lock: [{ rect: [4.5, 10.5, 15, 9.5, 2] }, 'M8 10.5V7.5a4 4 0 0 1 8 0v3'],
};

/** Dateikategorie → Symbolname. Ersetzt die früheren Emoji. */
const KATEGORIE_ICON = {
  installer: 'package',
  archive: 'archive',
  image: 'image',
  video: 'video',
  audio: 'music',
  pdf: 'fileText',
  document: 'fileText',
  spreadsheet: 'fileText',
  presentation: 'monitor',
  code: 'fileCode',
  torrent: 'transfer',
  temp: 'hourglass',
  font: 'font',
  ebook: 'book',
  disk_image: 'disc',
  shortcut: 'link',
  unknown: 'file',
};

/** Bekannter Windows-Ordner → Symbolname. */
const ORDNER_ICON = {
  downloads: 'download',
  documents: 'fileText',
  pictures: 'image',
  videos: 'video',
  music: 'music',
  desktop: 'monitor',
};

export function iconFuerKategorie(kategorie) {
  return KATEGORIE_ICON[kategorie] || 'file';
}

export function iconFuerOrdner(key) {
  return ORDNER_ICON[key] || 'folder';
}

/** Namen aller vorhandenen Symbole - für Tests. */
export function alleIconNamen() {
  return Object.keys(FORMEN);
}

/**
 * Erzeugt ein SVG-Symbol.
 *
 * Symbole sind immer schmückend: Sie tragen `aria-hidden`, weil die Bedeutung
 * aus der Beschriftung daneben kommen muss. Für Schaltflächen ohne Text sorgt
 * der Aufrufer für ein `aria-label`.
 *
 * @param {string} name  Schlüssel aus FORMEN
 * @param {{size?:number, strich?:number, klasse?:string}} [opts]
 */
export function icon(name, opts = {}) {
  const teile = FORMEN[name];
  const size = opts.size ?? 18;
  const svg = document.createElementNS(NS, 'svg');
  svg.setAttribute('viewBox', '0 0 24 24');
  svg.setAttribute('width', String(size));
  svg.setAttribute('height', String(size));
  svg.setAttribute('fill', 'none');
  svg.setAttribute('stroke', 'currentColor');
  svg.setAttribute('stroke-width', String(opts.strich ?? 1.75));
  svg.setAttribute('stroke-linecap', 'round');
  svg.setAttribute('stroke-linejoin', 'round');
  svg.setAttribute('aria-hidden', 'true');
  svg.setAttribute('focusable', 'false');
  svg.classList.add('icon');
  if (opts.klasse) svg.classList.add(opts.klasse);

  if (!teile) {
    // Ein fehlendes Symbol darf die Oberfläche nicht zerstören; es fällt aber
    // sichtbar auf, damit es beim Durchsehen der Screenshots auffliegt.
    const r = document.createElementNS(NS, 'rect');
    r.setAttribute('x', '4');
    r.setAttribute('y', '4');
    r.setAttribute('width', '16');
    r.setAttribute('height', '16');
    r.setAttribute('rx', '3');
    svg.append(r);
    return svg;
  }

  for (const teil of teile) {
    if (typeof teil === 'string') {
      const p = document.createElementNS(NS, 'path');
      p.setAttribute('d', teil);
      svg.append(p);
    } else if (teil.circle) {
      const [cx, cy, r] = teil.circle;
      const c = document.createElementNS(NS, 'circle');
      c.setAttribute('cx', String(cx));
      c.setAttribute('cy', String(cy));
      c.setAttribute('r', String(r));
      svg.append(c);
    } else if (teil.rect) {
      const [x, y, w, h, rx] = teil.rect;
      const el = document.createElementNS(NS, 'rect');
      el.setAttribute('x', String(x));
      el.setAttribute('y', String(y));
      el.setAttribute('width', String(w));
      el.setAttribute('height', String(h));
      el.setAttribute('rx', String(rx ?? 2));
      svg.append(el);
    }
  }
  return svg;
}
