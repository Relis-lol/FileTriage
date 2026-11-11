// Gemeinsame Listenansicht für "Große Dateien", "Alte Dateien" und die Suche.
// Alle drei sind dieselbe Tabelle mit anderen Voreinstellungen.

import * as api from '../lib/api.js';
import { btn, clear, dialog, el, iconBtn, toast } from '../lib/dom.js';
import { alter, bytes, datum, kategorie, kurzerPfad } from '../lib/format.js';
import { icon, iconFuerKategorie } from '../lib/icons.js';
import { neuZeichnen, store } from '../app.js';
import { t } from '../lib/i18n.js';

const SEITE = 500;

let abfrage = null;
let auswahl = new Set();
let host = null;

function grundAbfrage(view, params) {
  const cfg = store.config;
  const jetzt = Math.floor(Date.now() / 1000);
  const basis = { root: store.root, limit: SEITE, offset: 0, ...params };
  if (view === 'large') {
    return { ...basis, min_size: cfg.large_file_mb * 1024 * 1024, sort: 'size', desc: true };
  }
  if (view === 'old') {
    return { ...basis, modified_before: jetzt - cfg.old_file_days * 86400, sort: 'modified', desc: false };
  }
  return { ...basis, sort: 'modified', desc: true };
}

export async function render({ view, params, host: wirt }) {
  host = wirt;
  abfrage = grundAbfrage(view, params);
  auswahl = new Set();
  const wurzel = el('div');
  await zeichne(view, wurzel);
  return wurzel;
}

const KOPFTEXT = {
  large: {
    icon: 'drive',
    titel: 'files.title_large',
    unter: 'files.subtitle_large',
  },
  old: {
    icon: 'clock',
    titel: 'files.title_old',
    unter: 'files.subtitle_old',
  },
  search: { icon: 'search', titel: 'files.title_search', unter: 'files.subtitle_search' },
};

async function zeichne(view, wurzel) {
  clear(wurzel);
  const kopftext = KOPFTEXT[view] ?? KOPFTEXT.search;
  wurzel.append(
    el('div', { style: 'margin-bottom:var(--s-4)' }, [
      el('h2', { style: 'font-size:var(--t-xl)', text: t(kopftext.titel) }),
      el('p.muted', { style: 'margin:var(--s-1) 0 0', text: t(kopftext.unter) }),
    ]),
    kopf(view, wurzel)
  );
  const inhalt = el('div', { style: 'margin-top:var(--s-4)' });
  wurzel.append(inhalt);

  let seite;
  try {
    seite = await api.listFiles(abfrage);
  } catch (e) {
    inhalt.append(
      el('div.empty', {}, [
        el('div.empty-mark', {}, [icon('alertTriangle', { size: 26 })]),
        el('h2', { text: e?.title ?? t('files.load_failed') }),
        el('p', { text: e?.hint || e?.title || t('common.generic_failed') }),
      ])
    );
    return;
  }

  if (!seite.items.length) {
    inhalt.append(leerZustand(view));
    return;
  }

  inhalt.append(
    el('div.row.small.muted', { style: 'margin-bottom:8px' }, [
      el('span', {
        text: t('files.count_size', {
        count: t('home.file_count', { count: seite.total }),
        size: bytes(seite.total_size),
      }),
      }),
      seite.total > seite.items.length
        ? el('span', { text: t('files.showing_first', { count: seite.items.length }) })
        : null,
    ]),
    tabelle(seite, view, wurzel),
    aktionsLeiste(view, wurzel)
  );
}

function leerZustand(view) {
  const texte = {
    large: {
      icon: 'checkCircle',
      titel: 'files.empty_large_title',
      text: t('files.empty_large'),
    },
    old: {
      icon: 'checkCircle',
      titel: 'files.empty_old_title',
      text: t('files.empty_old'),
    },
    search: {
      icon: 'search',
      titel: 'files.empty_search_title',
      text: t('files.empty_search'),
    },
  };
  const leertext = texte[view] ?? texte.search;
  return el('div.empty', {}, [
    el('div.empty-mark', {}, [icon(leertext.icon, { size: 26 })]),
    el('h2', { text: t(leertext.titel) }),
    el('p', { text: leertext.text }),
  ]);
}

function kopf(view, wurzel) {
  const zeile = el('div.row');

  if (view === 'search') {
    const feld = el('input', {
      type: 'search',
      placeholder: t('files.search_placeholder'),
      value: abfrage.text ?? '',
      style: 'flex:1;min-width:220px',
      autofocus: true,
    });
    let timer = null;
    feld.addEventListener('input', () => {
      clearTimeout(timer);
      timer = setTimeout(() => {
        abfrage = { ...abfrage, text: feld.value, offset: 0 };
        zeichne(view, wurzel);
      }, 180);
    });
    zeile.append(feld);
    zeile.append(
      auswahlFeld(
        t('files.filter_type'),
        [['', t('files.filter_all')]].concat(
          ['image', 'video', 'pdf', 'document', 'archive', 'installer', 'code', 'audio', 'temp'].map(
            (c) => [c, kategorie(c)]
          )
        ),
        abfrage.categories?.[0] ?? '',
        (wert) => {
          abfrage = { ...abfrage, categories: wert ? [wert] : [], offset: 0 };
          zeichne(view, wurzel);
        }
      )
    );
  }

  if (view === 'large') {
    zeile.append(
      auswahlFeld(
        t('dupe.min_size'),
        [
          [10, bytes(10 * 1024 * 1024)],
          [50, bytes(50 * 1024 * 1024)],
          [100, bytes(100 * 1024 * 1024)],
          [500, bytes(500 * 1024 * 1024)],
          [1024, bytes(1024 * 1024 * 1024)],
        ],
        Math.round(abfrage.min_size / 1024 / 1024),
        (wert) => {
          abfrage = { ...abfrage, min_size: Number(wert) * 1024 * 1024, offset: 0 };
          zeichne(view, wurzel);
        }
      )
    );
  }

  if (view === 'old') {
    const jetzt = Math.floor(Date.now() / 1000);
    zeile.append(
      auswahlFeld(
        t('files.filter_old'),
        [
          [180, t('files.age_180')],
          [365, t('files.age_365')],
          [730, t('files.age_730')],
          [1825, t('files.age_1825')],
        ],
        Math.round((jetzt - abfrage.modified_before) / 86400),
        (wert) => {
          abfrage = { ...abfrage, modified_before: jetzt - Number(wert) * 86400, offset: 0 };
          zeichne(view, wurzel);
        }
      )
    );
  }

  zeile.append(
    el('label.field', {}, [
      el('span', { text: t('common.folder') }),
      el('span.small', { text: store.root ? kurzerPfad(store.root, 40) : t('files.no_folder') }),
    ])
  );
  return zeile;
}

function auswahlFeld(label, optionen, wert, onChange) {
  const sel = el(
    'select',
    { onChange: (e) => onChange(e.target.value) },
    optionen.map(([v, t]) => el('option', { value: v, text: t, selected: String(v) === String(wert) }))
  );
  return el('label.field', {}, [el('span', { text: label }), sel]);
}

function tabelle(seite, view, wurzel) {
  const spalten = [
    ['name', t('files.column_name')],
    ['ext', t('common.type')],
    ['size', t('files.column_size'), true],
    ['modified', t('common.modified')],
    ['parent', t('common.folder')],
  ];

  const kopfZeile = el('tr', {}, [
    el('th', { style: 'width:28px' }, [
      el('input', {
        type: 'checkbox',
        title: t('files.select_all'),
        onChange: (e) => {
          auswahl = e.target.checked ? new Set(seite.items.map((f) => f.path)) : new Set();
          zeichne(view, wurzel);
        },
      }),
    ]),
    ...spalten.map(([key, titel, rechts]) =>
      el('th', {
        class: rechts ? 'right' : '',
        'aria-sort':
          abfrage.sort === key ? (abfrage.desc ? 'descending' : 'ascending') : 'none',
        onClick: () => {
          abfrage =
            abfrage.sort === key
              ? { ...abfrage, desc: !abfrage.desc }
              : { ...abfrage, sort: key, desc: true };
          zeichne(view, wurzel);
        },
      }, [
        el('span.mit-icon', { style: rechts ? 'justify-content:flex-end' : '' }, [
          el('span', { text: titel }),
          // Die Sortierrichtung als Symbol, nicht als Unicode-Pfeil im Text.
          abfrage.sort === key
            ? icon('chevronDown', {
                size: 13,
                klasse: abfrage.desc ? '' : 'gedreht',
              })
            : null,
        ]),
      ])
    ),
    el('th', { style: 'width:76px' }, [el('span.sr-only', { text: t('common.actions') })]),
  ]);

  const zeilen = seite.items.map((f) =>
    el(
      'tr',
      {
        class: auswahl.has(f.path) ? 'selected' : '',
        onDblclick: () =>
          api.revealPath(f.path).catch((e) => toast(e?.title ?? t('common.show_failed'), 'error')),
      },
      [
        el('td', {}, [
          el('input', {
            type: 'checkbox',
            checked: auswahl.has(f.path),
            onChange: (e) => {
              if (e.target.checked) auswahl.add(f.path);
              else auswahl.delete(f.path);
              e.target.closest('tr').classList.toggle('selected', e.target.checked);
              aktualisiereAktionen();
            },
          }),
        ]),
        el('td', { title: f.path }, [
          el('span.mit-icon', {}, [
            icon(iconFuerKategorie(f.category), { size: 15 }),
            el('span.truncate', { text: f.name }),
          ]),
        ]),
        el('td', { text: f.ext ? `.${f.ext}` : kategorie(f.category) }),
        el('td.right', { text: bytes(f.size) }),
        el('td', { title: datum(f.modified), text: alter(f.modified) }),
        el('td', { title: f.parent, text: kurzerPfad(f.parent, 34) }),
        el('td', { style: 'width:76px' }, [
          el('div.row', { style: 'gap:2px;flex-wrap:nowrap' }, [
            iconBtn('externalLink', t('common.open_named', { name: f.name }), {
              kind: 'ghost',
              size: 'small',
              onClick: () => api.openPath(f.path).catch((e) => toast(e?.title ?? t('common.open_failed'), 'error')),
            }),
            iconBtn('folder', t('common.reveal_named', { name: f.name }), {
              kind: 'ghost',
              size: 'small',
              onClick: () => api.revealPath(f.path).catch((e) => toast(e?.title ?? t('common.show_failed'), 'error')),
            }),
          ]),
        ]),
      ]
    )
  );

  return el('table.files', {}, [el('thead', {}, [kopfZeile]), el('tbody', {}, zeilen)]);
}

function aktionsLeiste(view, wurzel) {
  const leiste = el('div.card.row', {
    id: 'aktionen',
    style: 'position:sticky;bottom:0;margin-top:12px',
  });
  aktualisiereAktionen(leiste, view, wurzel);
  return leiste;
}

let leisteRef = null;
let leisteKontext = null;

function aktualisiereAktionen(leiste, view, wurzel) {
  if (leiste) {
    leisteRef = leiste;
    leisteKontext = { view, wurzel };
  }
  if (!leisteRef) return;
  clear(leisteRef);
  const n = auswahl.size;
  leisteRef.append(
    el('span.small.muted', {
      text: n ? t('files.selected_count', { count: n }) : t('files.select_hint'),
    }),
    el('span', { style: 'flex:1' }),
    btn({
      icon: 'folderOpen',
      text: t('files.move_to_folder'),
      disabled: !n,
      onClick: () => verschiebeAuswahl(),
    }),
    btn({
      icon: 'trash',
      text: t('files.to_trash'),
      kind: 'danger',
      disabled: !n,
      onClick: () => loescheAuswahl(),
    })
  );
}

async function verschiebeAuswahl() {
  const pfade = [...auswahl];
  const ziel = await api.pickFolder(t('settings.pick_target'));
  if (!ziel) return;

  const ok = await dialog({
    title: t('files.confirm_move_title'),
    body: el('div', {}, [
      el('p', { text: t('files.confirm_move_body', { count: pfade.length, target: ziel }) }),
      el('div.list-preview', {}, pfade.slice(0, 40).map((p) => el('div', { text: p }))),
      pfade.length > 40 ? el('div.small.muted', { text: t('common.more_count', { count: pfade.length - 40 }) }) : null,
    ]),
    buttons: [
      { label: t('files.confirm_move_ok', { count: pfade.length }), value: true, kind: 'primary' },
      { label: t('common.cancel'), value: false },
    ],
  });
  if (!ok) return;

  try {
    const r = await api.moveMany(pfade, ziel, 'rename');
    meldeStapel(r, 'verschoben');
    auswahl = new Set();
    neuZeichnen();
  } catch (e) {
    toast(e?.title ?? t('common.action_failed'), 'error');
  }
}

async function loescheAuswahl() {
  const pfade = [...auswahl];
  const endgueltig = store.config?.delete_mode === 'permanent';
  const ok = await dialog({
    title: endgueltig ? t('triage.permanent_title') : t('files.delete_recycle_title'),
    body: el('div', {}, [
      el('p', {
        text: endgueltig
          ? t('files.delete_permanent_body', { count: pfade.length })
          : t('files.delete_recycle_body', { count: pfade.length }),
      }),
      el('div.list-preview', {}, pfade.slice(0, 40).map((p) => el('div', { text: p }))),
    ]),
    buttons: [
      { label: t('common.cancel'), value: false },
      { label: endgueltig ? t('triage.permanent_confirm') : t('files.to_trash'), value: true, kind: 'danger' },
    ],
  });
  if (!ok) return;

  try {
    const r = await api.deleteMany(pfade, endgueltig ? true : undefined);
    meldeStapel(r, endgueltig);
    auswahl = new Set();
    neuZeichnen();
  } catch (e) {
    toast(e?.title ?? t('common.action_failed'), 'error');
  }
}

function meldeStapel(r, endgueltig) {
  const teile = [t(endgueltig ? 'files.deleted' : 'files.trashed', { count: r.done })];
  if (r.skipped) teile.push(t('files.skipped', { count: r.skipped }));
  if (r.collisions) teile.push(t('files.collisions', { count: r.collisions }));
  if (r.failed) teile.push(t('files.failed', { count: r.failed }));
  toast(
    teile.join(' · '),
    r.failed ? 'error' : 'success',
    r.done
      ? {
          label: t('common.undo'),
          onClick: async () => {
            const ergebnisse = await api.undoBatch(r.batch_id);
            const gut = ergebnisse.filter((x) => x.ok).length;
            toast(t('files.undo_result', { done: gut, total: ergebnisse.length }));
            neuZeichnen();
          },
        }
      : null
  );
}
