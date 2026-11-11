// Die Triage — der wichtigste Bildschirm.
//
// Visuelle Reihenfolge, absichtlich in dieser Rangfolge:
//
//   1. die Datei selbst (große Vorschau)
//   2. Name, dann Typ · Größe · Datum in einer Zeile
//   3. wohin damit (Zielordner mit Zifferntaste)
//   4. Papierkorb, Überspringen, Öffnen
//   5. was zuletzt passiert ist, mit Rückgängig
//
// Alles Weitere — Pfad, Erstellungsdatum, EXIF — steckt hinter „Weitere
// Angaben". Zwanzig gleich laute Metadaten sind keine Information.

import * as api from '../lib/api.js';
import { btn, clear, dialog, el, fehlerDialog, toast } from '../lib/dom.js';
import { icon, iconFuerKategorie } from '../lib/icons.js';
import { alter, bytes, datumZeit, kategorie, kurzerPfad, taste } from '../lib/format.js';
import { gehZu, kaufen, ordnerWaehlen, store, undoLetzte } from '../app.js';
import { t } from '../lib/i18n.js';
import { zahl } from '../lib/i18n.js';

const STAPEL = 300;

const zustand = {
  warteschlange: [],
  index: 0,
  gesamt: 0,
  erledigt: 0,
  /** Was zuletzt passiert ist — für die Fußzeile. */
  letzte: null,
  bilanz: { verschoben: 0, papierkorb: 0, uebersprungen: 0, behalten: 0 },
};

let knoten = null;
let refs = { vorschau: null, exif: null, ziele: null };

/**
 * Der Bildschirm nach Ablauf der Testphase.
 *
 * Er ersetzt die Triage, nicht das Programm. Was hier ausdruecklich NICHT
 * passiert: nichts wird geloescht, keine Einstellung zurueckgesetzt, keine
 * Sitzung beendet, kein Journal geleert. Wer nicht kauft, hat seine Daten
 * weiter - er kann sie nur nicht mehr sortieren.
 *
 * Ansehen, Suchen, Duplikate pruefen und Rueckgaengig bleiben erreichbar. Eine
 * halb gesperrte Triage waere schlimmer als eine ganz gesperrte: Man wuesste
 * bei keinem Knopf, ob er noch geht.
 */
function testphaseAbgelaufen() {
  const preis = store.lizenz?.price_hint ?? '';
  return el('div.empty.lizenz-ende', {}, [
    el('div.empty-mark', {}, [icon('lock', { size: 26 })]),
    el('h2', { text: t('license.expired_title') }),
    el('p', { text: t('license.expired_body', { price: preis }) }),
    btn({
      icon: 'externalLink',
      text: t('license.buy_short'),
      kind: 'primary',
      size: 'large',
      onClick: kaufen,
    }),
    el('div.lizenz-zusagen', {}, [
      el('div.assurance', {}, [
        icon('shield', { size: 14 }),
        el('span', { text: t('license.expired_data_safe') }),
      ]),
      el('div.assurance', {}, [
        icon('eye', { size: 14 }),
        el('span', { text: t('license.expired_readonly') }),
      ]),
      el('div.assurance', {}, [
        icon('undo', { size: 14 }),
        el('span', { text: t('license.expired_undo') }),
      ]),
    ]),
  ]);
}

export async function render({ host }) {
  // Vor allem anderen: Ohne Recht, Dateien zu veraendern, hat die Triage
  // keinen Sinn. Die eigentliche Sperre sitzt im Kern - dies hier erspart dem
  // Nutzer nur, sie durch eine Fehlermeldung zu entdecken.
  if (store.lizenz?.channel !== 'community' && store.lizenz && !store.lizenz.may_modify) {
    return testphaseAbgelaufen();
  }

  if (!store.root) {
    return el('div.empty', {}, [
      el('div.empty-mark', {}, [icon('folderOpen', { size: 26 })]),
      el('h2', { text: t('triage.no_folder') }),
      el('p', { text: t('triage.no_folder_hint') }),
      btn({ icon: 'folderOpen', text: t('home.choose_folder_short'), kind: 'primary', onClick: ordnerWaehlen }),
    ]);
  }

  await ladeWarteschlange();
  host.classList.remove('view');
  host.classList.add('triage');
  store.keyHandler = beiTaste;

  knoten = el('div', { style: 'display:contents' });
  zeichne();
  return knoten;
}

async function ladeWarteschlange() {
  const seite = await api.listFiles({
    root: store.root,
    states: ['pending'],
    sort: 'modified',
    desc: true,
    limit: STAPEL,
  });
  zustand.warteschlange = seite.items;
  zustand.gesamt = seite.total;
  zustand.index = 0;
  store.offen = seite.total;
}

function aktuelle() {
  return zustand.warteschlange[zustand.index] || null;
}

function zeichne() {
  if (!knoten) return;
  clear(knoten);
  const datei = aktuelle();

  if (!datei) {
    knoten.append(abschluss());
    return;
  }

  knoten.append(kopfzeile(), karte(datei), fusszeile());
  ladeVorschau(datei);
  ladeVorschlag(datei);
}

// ---------------------------------------------------------------------------
// Kopf und Fuß
// ---------------------------------------------------------------------------

function kopfzeile() {
  const offen = Math.max(zustand.gesamt - zustand.erledigt, 0);
  const anteil = zustand.gesamt ? (zustand.erledigt / zustand.gesamt) * 100 : 0;

  return el('div.triage-head', {}, [
    el('span.zaehler', { text: zahl(offen) }),
    el('span.small.muted', { text: t('triage.remaining_label', { count: offen }) }),
    el(
      'div.progress',
      {
        role: 'progressbar',
        'aria-valuemin': '0',
        'aria-valuemax': String(zustand.gesamt),
        'aria-valuenow': String(zustand.erledigt),
        'aria-label': t('triage.progress_aria'),
      },
      [el('div', { style: `width:${anteil}%` })]
    ),
    el('span.small.muted', { text: t('triage.done_count', { count: zustand.erledigt }) }),
  ]);
}

function fusszeile() {
  const l = zustand.letzte;
  return el('div.lastbar', {}, [
    l
      ? el('span.was', {}, [
          icon(l.icon, { size: 15 }),
          el('span.truncate', { title: l.text, text: l.text }),
        ])
      : el('span.assurance', {}, [
          icon('shield', { size: 14 }),
          el('span', { text: t('triage.footer') }),
        ]),
    el('div.grow'),
    btn({
      icon: 'undo',
      text: t('common.undo'),
      size: 'small',
      kind: 'ghost',
      hotkey: taste(store.config?.hotkeys?.undo ?? 'Backspace'),
      disabled: !l,
      onClick: zurueck,
    }),
  ]);
}

/** Sitzungsabschluss statt einer leeren Fläche. */
function abschluss() {
  const b = zustand.bilanz;
  const etwasPassiert = zustand.erledigt > 0;

  return el('div.empty', {}, [
    el('div.empty-mark', {
      style: etwasPassiert ? 'background:var(--ok-soft);color:var(--ok)' : '',
    }, [icon(etwasPassiert ? 'checkCircle' : 'inbox', { size: 26 })]),
    el('h2', { text: etwasPassiert ? t('triage.finished_title') : t('triage.finished_empty_title') }),
    el('p', {
      text: etwasPassiert
        ? t('triage.finished_body')
        : t('triage.finished_empty_body'),
    }),

    etwasPassiert
      ? el('div.card.flat', { style: 'width:100%;text-align:left' }, [
          bilanzZeile('arrowRight', b.verschoben, 'triage.summary_moved'),
          bilanzZeile('trash', b.papierkorb, 'triage.summary_trashed'),
          bilanzZeile('skip', b.uebersprungen, 'triage.summary_skipped'),
          bilanzZeile('check', b.behalten, 'triage.summary_kept'),
        ])
      : null,

    el('div.row', { style: 'justify-content:center;margin-top:var(--s-2)' }, [
      btn({
        icon: 'refresh',
        text: t('scan.rescan'),
        onClick: async () => {
          await ladeWarteschlange();
          zeichne();
        },
      }),
      btn({ icon: 'home', text: t('triage.to_overview'), kind: 'primary', onClick: () => gehZu('home') }),
    ]),
  ]);
}

function bilanzZeile(name, zahl, key) {
  if (!zahl) return null;
  return el('div.row', { style: 'padding:var(--s-1) 0' }, [
    icon(name, { size: 15 }),
    el('span.muted', { text: t(key, { count: zahl }) }),
  ]);
}

// ---------------------------------------------------------------------------
// Die Karte
// ---------------------------------------------------------------------------

function karte(datei) {
  const vorschau = el('div.preview-pane', {}, [platzhalter(datei, t('triage.preview_loading'))]);
  const exif = el('div');
  const ziele = zielListe(datei);
  refs = { vorschau, exif, ziele };

  const h = store.config?.hotkeys ?? {};

  const seite = el('div.file-pane', {}, [
    el('div', {}, [
      el('div.file-name', { title: datei.name, text: datei.name }),
      el('div.file-facts', { style: 'margin-top:var(--s-1)' }, [
        el('span', { text: datei.ext ? `.${datei.ext}` : kategorie(datei.category) }),
        el('span.trenner', { text: '·' }),
        el('span', { text: bytes(datei.size) }),
        el('span.trenner', { text: '·' }),
        el('span', { title: datumZeit(datei.modified), text: t('triage.changed_ago', { when: alter(datei.modified) }) }),
      ]),
    ]),

    ziele,

    el('div.triage-actions', {}, [
      btn({
        icon: 'trash',
        text: t('triage.trash'),
        kind: 'danger',
        hotkey: taste(h.delete ?? 'd'),
        onClick: () => loeschen(datei),
      }),
      btn({
        icon: 'skip',
        text: t('triage.skip'),
        hotkey: taste(h.skip ?? 's'),
        onClick: () => markieren(datei, 'skipped', 'uebersprungen'),
      }),
      btn({
        icon: 'externalLink',
        text: t('common.open'),
        hotkey: taste(h.open ?? 'o'),
        onClick: () => api.openPath(datei.path).catch(fehler),
      }),
    ]),

    // Alles, was selten gebraucht wird, aber manchmal entscheidend ist.
    el('details.details', {}, [
      el('summary', { text: t('triage.more_facts') }),
      el('div', { style: 'padding-top:var(--s-2)' }, [
        el('dl.meta', {}, [
          el('dt', { text: t('common.folder') }),
          el('dd', { title: datei.parent, text: kurzerPfad(datei.parent, 40) }),
          el('dt', { text: t('common.type') }),
          el('dd', { text: kategorie(datei.category) }),
          el('dt', { text: t('common.modified') }),
          el('dd', { text: datumZeit(datei.modified) }),
          el('dt', { text: t('common.created') }),
          el('dd', { text: datumZeit(datei.created) }),
        ]),
        exif,
        el('div.row', { style: 'margin-top:var(--s-3)' }, [
          btn({
            icon: 'folder',
            text: t('triage.reveal'),
            size: 'small',
            hotkey: taste(h.explorer ?? 'e'),
            onClick: () => api.revealPath(datei.path).catch(fehler),
          }),
          btn({
            icon: 'pencil',
            text: t('common.rename'),
            size: 'small',
            hotkey: taste(h.rename ?? 'r'),
            onClick: () => umbenennen(datei),
          }),
          btn({
            icon: 'check',
            text: t('triage.keep'),
            size: 'small',
            hotkey: taste(h.keep ?? 'k'),
            onClick: () => markieren(datei, 'kept', 'behalten'),
          }),
        ]),
      ]),
    ]),
  ]);

  return el('div.triage-card', {}, [vorschau, seite]);
}

/** Großes Typ-Symbol statt kaputter Fläche. */
function platzhalter(datei, text) {
  return el('div.preview-empty', {}, [
    el('div.typ', {}, [icon(iconFuerKategorie(datei.category), { size: 40, strich: 1.4 })]),
    datei.ext ? el('div.endung', { text: datei.ext }) : null,
    el('div.small', { text }),
  ]);
}

function zielListe(datei) {
  const ziele = store.config?.targets ?? [];
  if (!ziele.length) {
    return el('div.card.flat', {}, [
      el('div.small.muted', { text: t('triage.no_targets') }),
      btn({
        icon: 'folderPlus',
        text: t('triage.setup_targets'),
        size: 'small',
        onClick: () => gehZu('settings'),
      }),
    ]);
  }

  return el('div', {}, [
    el('div.section-title', { style: 'margin:0 0 var(--s-2)' }, [
      icon('arrowRight', { size: 14 }),
      el('span', { text: t('triage.move_to') }),
    ]),
    el(
      'div.targets',
      {},
      ziele.map((z) =>
        el(
          'button.target-btn',
          {
            type: 'button',
            dataset: { key: z.key },
            'aria-keyshortcuts': z.key,
            title: z.path,
            onClick: () => verschieben(datei, z.key),
          },
          [
            el('kbd', { text: z.key, 'aria-hidden': 'true' }),
            icon('folder', { size: 16 }),
            el('span.labels', {}, [
              el('span.name', { text: z.label }),
              el('span.path', { text: kurzerPfad(z.path, 30) }),
            ]),
          ]
        )
      )
    ),
  ]);
}

// ---------------------------------------------------------------------------
// Vorschau
// ---------------------------------------------------------------------------

async function ladeVorschau(datei) {
  const ziel = refs.vorschau;
  if (!ziel) return;
  try {
    const v = await api.preview(datei.path);
    if (aktuelle()?.path !== datei.path) return;
    clear(ziel);

    if (v.kind === 'image' && v.data_url) {
      ziel.append(el('img', { src: v.data_url, alt: t('triage.preview_alt', { name: datei.name }) }));
      if (refs.exif && (v.width || v.meta.length)) {
        clear(refs.exif);
        refs.exif.append(
          el('div.row', { style: 'margin-top:var(--s-2)' }, [
            v.width ? el('span.pill', { text: t('triage.dimensions', { width: v.width, height: v.height }) }) : null,
            ...v.meta.map(([k, wert]) => el('span.pill', {
              text: t('common.pair', { label: t(`preview.meta.${k}`), value: wert }),
            })),
          ])
        );
      }
    } else if (v.kind === 'text' && v.text) {
      ziel.append(el('pre', { text: v.text + (v.truncated ? `\n\n${t('triage.preview_truncated')}` : '') }));
    } else {
      // Keine Vorschau ist kein Fehler.
      ziel.append(platzhalter(
        datei,
        v.note_code ? t(`preview.note.${v.note_code}`) : t('triage.no_preview')
      ));
    }
  } catch (e) {
    clear(ziel);
    ziel.append(platzhalter(datei, e?.title ?? t('triage.preview_failed')));
  }
}

/** Passiver Zielvorschlag — markiert einen Knopf, unterbricht aber nichts. */
async function ladeVorschlag(datei) {
  if (store.config?.suggestions_enabled === false) return;
  try {
    const v = await api.suggestTarget(datei.path);
    if (!v || !v.key || aktuelle()?.path !== datei.path) return;
    const knopf = refs.ziele?.querySelector(`.target-btn[data-key="${v.key}"]`);
    if (!knopf || knopf.querySelector('.suggest-tag')) return;
    knopf.classList.add('suggested');
    const reason = t(`triage.suggestion_reason.${v.reason_code}`, {
      name: v.rule_name ?? '',
      count: v.count ?? 0,
    });
    knopf.title = `${reason} — ${knopf.title}`;
    knopf.append(el('span.suggest-tag', { text: t('triage.suggestion') }));
  } catch {
    // Vorschläge sind Beiwerk. Fehler bleiben still.
  }
}

// ---------------------------------------------------------------------------
// Aktionen
// ---------------------------------------------------------------------------

function fehler(e) {
  fehlerDialog(e);
}

function ergebnisText(r) {
  if (r?.status === 'skipped') return t('triage.skipped_status');
  if (r?.error_kind) {
    const key = `errors.${r.error_kind}.title`;
    const value = t(key);
    if (value !== key) return value;
  }
  return t('common.generic_failed');
}

function merkeLetzte(iconName, text) {
  zustand.letzte = { icon: iconName, text };
}

function weiter() {
  zustand.erledigt += 1;
  zustand.warteschlange.splice(zustand.index, 1);
  if (zustand.index >= zustand.warteschlange.length) {
    zustand.index = zustand.warteschlange.length - 1;
  }
  if (zustand.index < 0) zustand.index = 0;
  store.offen = Math.max(zustand.gesamt - zustand.erledigt, 0);
  zeichne();
}

async function zurueck() {
  await undoLetzte();
  zustand.letzte = null;
  zustand.erledigt = Math.max(zustand.erledigt - 1, 0);
  await ladeWarteschlange();
  zeichne();
}

async function verschieben(datei, zielKey) {
  const ziel = store.config?.targets?.find((t) => t.key === zielKey);
  try {
    const r = await api.moveToTarget(datei.path, zielKey, 'ask');
    if (r.status === 'done') {
      zustand.bilanz.verschoben += 1;
      merkeLetzte(
        'arrowRight',
        t('triage.last_moved', { name: datei.name, target: ziel?.label ?? t('common.target') })
      );
      weiter();
    } else if (r.status === 'collision') {
      await kollisionLoesen(datei, zielKey, r);
    } else if (r.status === 'skipped') {
      toast(ergebnisText(r));
      weiter();
    } else {
      fehlerDialog(
        { title: t('triage.move_failed'), hint: ergebnisText(r), message: r.message },
        [{ label: t('common.understood'), value: null, kind: 'primary' }]
      );
    }
  } catch (e) {
    fehler(e);
  }
}

async function kollisionLoesen(datei, zielKey, ergebnis) {
  const vorhanden = ergebnis.existing;
  const koerper = el('div', {}, [
    el('p', { text: t('triage.collision_body') }),
    el('div.list-preview', {}, [
      el('div', { text: t('triage.collision_this', {
        size: bytes(datei.size),
        when: datumZeit(datei.modified),
      }) }),
      el('div', { text: t('triage.collision_there', {
        size: bytes(vorhanden.size),
        when: datumZeit(vorhanden.modified),
      }) }),
    ]),
  ]);

  const wahl = await dialog({
    title: t('triage.collision_title'),
    body: koerper,
    buttons: [
      { label: t('triage.collision_rename', { name: vorhanden.suggested_name }), value: 'rename', kind: 'primary' },
      { label: t('triage.collision_skip'), value: 'skip' },
      { label: t('triage.collision_show'), value: 'reveal' },
      { label: t('common.cancel'), value: null },
    ],
  });

  if (wahl === 'rename') {
    const r = await api.moveToTarget(datei.path, zielKey, 'rename');
    if (r.status === 'done') {
      zustand.bilanz.verschoben += 1;
      merkeLetzte('arrowRight', t('triage.last_moved', { name: datei.name, target: r.dst.split('\\').pop() }));
      weiter();
    } else {
      fehlerDialog({ title: t('triage.move_failed'), hint: ergebnisText(r), message: r.message });
    }
  } else if (wahl === 'skip') {
    await api.setFileState(datei.path, 'skipped').catch(fehler);
    zustand.bilanz.uebersprungen += 1;
    merkeLetzte('skip', t('triage.last_skipped', { name: datei.name }));
    weiter();
  } else if (wahl === 'reveal') {
    api.revealPath(vorhanden.path).catch(fehler);
  }
}

async function loeschen(datei) {
  const endgueltig = store.config?.delete_mode === 'permanent';

  // Der Papierkorb braucht keine Rückfrage — er ist der Rückweg.
  // Endgültiges Löschen bekommt eine, und zwar eine deutliche.
  if (endgueltig) {
    const ok = await dialog({
      title: t('triage.permanent_title'),
      body: el('div', {}, [
        el('p', {
          text: t('triage.permanent_body', { name: datei.name }),
        }),
        el('p.small.muted', {
          text: t('triage.permanent_hint'),
        }),
      ]),
      buttons: [
        { label: t('common.cancel'), value: false },
        { label: t('triage.permanent_confirm'), value: true, kind: 'danger' },
      ],
    });
    if (!ok) return;
  }

  try {
    const r = await api.deleteFile(datei.path, endgueltig ? true : undefined);
    if (r.status === 'done') {
      zustand.bilanz.papierkorb += 1;
      merkeLetzte(
        'trash',
        t(endgueltig ? 'triage.last_deleted' : 'triage.last_trashed', { name: datei.name })
      );
      weiter();
    } else {
      fehlerDialog({ title: t('triage.delete_failed'), hint: ergebnisText(r), message: r.message });
    }
  } catch (e) {
    fehler(e);
  }
}

async function umbenennen(datei) {
  const feld = el('input', { type: 'text', value: datei.name, style: 'width:100%' });
  const ok = await dialog({
    title: t('common.rename'),
    body: feld,
    buttons: [
      { label: t('common.rename'), value: true, kind: 'primary' },
      { label: t('common.cancel'), value: false },
    ],
  });
  if (!ok) return;
  const r = await api.renameFile(datei.path, feld.value).catch((e) => {
    fehler(e);
    return null;
  });
  if (!r) return;
  if (r.status === 'done') {
    merkeLetzte('pencil', t('triage.last_renamed', { name: feld.value }));
    await ladeWarteschlange();
    zeichne();
  } else {
    fehlerDialog({ title: t('triage.rename_failed'), hint: ergebnisText(r), message: r.message });
  }
}

async function markieren(datei, neuerZustand, bilanzFeld) {
  try {
    await api.setFileState(datei.path, neuerZustand);
    zustand.bilanz[bilanzFeld] += 1;
    merkeLetzte(
      neuerZustand === 'kept' ? 'check' : 'skip',
      t(neuerZustand === 'kept' ? 'triage.last_kept' : 'triage.last_skipped', {
        name: datei.name,
      })
    );
    weiter();
  } catch (e) {
    fehler(e);
  }
}

function beiTaste(event) {
  const datei = aktuelle();
  if (!datei) return;
  const h = store.config?.hotkeys ?? {};
  const k = event.key;

  const zielTreffer = Object.entries(h).find(
    ([aktion, key]) => aktion.startsWith('target') && key === k
  );
  if (zielTreffer) {
    event.preventDefault();
    const nummer = zielTreffer[0].replace('target', '');
    if (store.config.targets.some((z) => z.key === nummer)) verschieben(datei, nummer);
    return;
  }

  switch (k) {
    case h.delete ?? 'd':
      event.preventDefault();
      loeschen(datei);
      break;
    case h.skip ?? 's':
      event.preventDefault();
      markieren(datei, 'skipped', 'uebersprungen');
      break;
    case h.keep ?? 'k':
      event.preventDefault();
      markieren(datei, 'kept', 'behalten');
      break;
    case h.open ?? 'o':
      event.preventDefault();
      api.openPath(datei.path).catch(fehler);
      break;
    case h.explorer ?? 'e':
      event.preventDefault();
      api.revealPath(datei.path).catch(fehler);
      break;
    case h.rename ?? 'r':
      event.preventDefault();
      umbenennen(datei);
      break;
    case h.next ?? 'ArrowRight':
      event.preventDefault();
      if (zustand.index < zustand.warteschlange.length - 1) {
        zustand.index += 1;
        zeichne();
      }
      break;
    case h.prev ?? 'ArrowLeft':
      event.preventDefault();
      if (zustand.index > 0) {
        zustand.index -= 1;
        zeichne();
      }
      break;
    default:
      break;
  }
}
