// Duplikate: erst suchen, dann Gruppen zeigen. Ausgewählt wird nie automatisch.

import * as api from '../lib/api.js';
import { btn, clear, dialog, el, iconBtn, toast } from '../lib/dom.js';
import { icon, iconFuerKategorie } from '../lib/icons.js';
import { bytes, datum, kurzerPfad } from '../lib/format.js';
import { neuZeichnen, store } from '../app.js';
import { t } from '../lib/i18n.js';
import { zahl } from '../lib/i18n.js';

// Nur die Schwellen stehen hier. Die Beschriftung entsteht beim Zeichnen aus
// dem Katalog und der Groessenformatierung der eingestellten Sprache - "ab
// 1 MB" fest einzutragen waere gleich zweimal deutsch.
const GROESSEN = [1, 1024 * 1024, 10 * 1024 * 1024, 100 * 1024 * 1024];

function groessenLabel(schwelle) {
  return schwelle <= 1 ? t('dupe.any_size') : t('dupe.from_size', { size: bytes(schwelle) });
}

let minGroesse = 1024 * 1024;
let entfernen = new Set();
let abmelden = null;

export async function render() {
  entfernen = new Set();
  const wurzel = el('div');
  wurzel.append(
    el('div', { style: 'margin-bottom:var(--s-4)' }, [
      el('h2', { style: 'font-size:var(--t-xl)', text: t('dupe.title') }),
      el('p.muted', {
        style: 'margin:var(--s-1) 0 0',
        text: t('dupe.intro'),
      }),
    ])
  );
  const kopf = el('div.row');
  const status = el('div', { style: 'margin:12px 0' });
  const liste = el('div');
  wurzel.append(kopf, status, liste);

  const zeichneKopf = (laeuft) => {
    clear(kopf);
    kopf.append(
      el('label.field', {}, [
        el('span', { text: t('dupe.min_size') }),
        el(
          'select',
          {
            onChange: (e) => {
              minGroesse = Number(e.target.value);
              zeigeGruppen();
            },
          },
          GROESSEN.map((v) =>
            el('option', { value: v, text: groessenLabel(v), selected: v === minGroesse })
          )
        ),
      ]),
      laeuft
        ? btn({ icon: 'x', text: t('common.cancel'), onClick: () => api.cancelDuplicates() })
        : btn({ icon: 'search', text: t('dupe.search'), kind: 'primary', onClick: suchen })
    );
  };

  const zeigeFortschritt = (p) => {
    clear(status);
    if (!p) return;
    const phasen = {
      1: t('dupe.stage1'),
      2: t('dupe.stage2'),
      3: t('dupe.stage3'),
    };
    const anteil = p.total ? Math.round((p.checked / p.total) * 100) : 0;
    status.append(
      el('div.card', {}, [
        el('div.row', {}, [
          el('span', { text: phasen[p.phase] || t('dupe.searching') }),
          el('span.spacer', { style: 'flex:1' }),
          el('span.small.muted', {
            text: p.total ? `${zahl(p.checked)} / ${zahl(p.total)}` : '',
          }),
        ]),
        el('div.progress', { style: 'margin-top:8px' }, [el('div', { style: `width:${anteil}%` })]),
      ])
    );
  };

  const zeigeGruppen = async () => {
    clear(liste);
    let gruppen;
    try {
      gruppen = await api.duplicateGroups(store.root, minGroesse);
    } catch (e) {
      liste.append(el('div.empty', { text: e?.title || t('common.generic_failed') }));
      return;
    }
    if (!gruppen.length) {
      liste.append(
        el('div.empty', {}, [
          el('div.empty-mark', {}, [icon('checkCircle', { size: 26 })]),
          el('h2', { text: t('dupe.none_title') }),
          el('p', {
            text: t('dupe.none_body'),
          }),
        ])
      );
      return;
    }

    const verschwendet = gruppen.reduce((s, g) => s + g.wasted, 0);
    liste.append(
      el('div.row.small.muted', { style: 'margin-bottom:10px' }, [
        el('span', {
          text: t('dupe.summary', {
          groups: t('dupe.group_count', { count: gruppen.length }),
          size: bytes(verschwendet),
        }),
        }),
      ])
    );

    for (const g of gruppen) liste.append(gruppenKarte(g));
    liste.append(aktionsLeiste());
  };

  const suchen = async () => {
    if (!store.root) {
      toast(t('dupe.choose_folder_first'), 'error');
      return;
    }
    try {
      await api.findDuplicates(store.root, minGroesse);
      zeichneKopf(true);
    } catch (e) {
      toast(e?.title || t('common.generic_failed'), 'error');
    }
  };

  if (abmelden) abmelden();
  abmelden = await api.listen('dup:progress', ({ payload }) => {
    zeigeFortschritt(payload);
    zeichneKopf(!payload.done);
    if (payload.done) {
      clear(status);
      zeigeGruppen();
      if (!payload.cancelled) {
        toast(
          payload.groups
            ? t('dupe.found', {
                groups: t('dupe.group_count', { count: payload.groups }),
                size: bytes(payload.wasted),
              })
            : t('dupe.none_title'),
          'success'
        );
      }
    }
  });

  const status0 = await api.duplicateStatus();
  zeichneKopf(status0.running);
  if (status0.running) zeigeFortschritt(status0.progress);
  await zeigeGruppen();
  return wurzel;
}

function gruppenKarte(g) {
  const karte = el('div.dupe-group');
  karte.append(
    el('div.dupe-head', {}, [
      icon('copy', { size: 16 }),
      el('strong', { text: t('dupe.same_file', { count: g.files.length }) }),
      el('span.pill', { text: bytes(g.size) }),
      g.hardlinked
        ? el('span.pill', {
            title:
              t('dupe.hardlinked_hint'),
            text: t('dupe.hardlinked'),
          })
        : el('span.pill.warn', { text: t('dupe.free', { size: bytes(g.wasted) }) }),
      el('span', { style: 'flex:1' }),
      btn({
        icon: 'check',
        text: t('dupe.select_rest'),
        size: 'small',
        title: t('dupe.select_hint'),
        onClick: () => {
          g.files.slice(1).forEach((f) => entfernen.add(f.path));
          karte.querySelectorAll('input[type=checkbox]').forEach((box, i) => {
            box.checked = i > 0;
          });
          aktualisiereLeiste();
        },
      }),
    ])
  );

  g.files.forEach((f, i) => {
    karte.append(
      el('div.dupe-file', {}, [
        el('input', {
          type: 'checkbox',
          checked: entfernen.has(f.path),
          onChange: (e) => {
            if (e.target.checked) entfernen.add(f.path);
            else entfernen.delete(f.path);
            aktualisiereLeiste();
          },
        }),
        el('span.dupe-thumb', {}, [icon(iconFuerKategorie(f.category), { size: 18 })]),
        el('span.path', {}, [
          el('div', { text: f.name }),
          el('div.xs.dim', { title: f.path, text: kurzerPfad(f.parent, 52) }),
        ]),
        i === 0 ? el('span.pill.ok', { text: t('dupe.keep_suggested') }) : null,
        el('span.small.muted', { text: datum(f.modified) }),
        iconBtn('folder', t('common.reveal_named', { name: f.name }), {
          kind: 'ghost',
          size: 'small',
          onClick: () => api.revealPath(f.path).catch((e) => toast(e?.title ?? t('common.show_failed'), 'error')),
        }),
      ])
    );
  });

  // Der Hash beweist die Gleichheit, aber niemand liest ihn beim Aufräumen.
  karte.append(
    el('details.details', { style: 'padding:var(--s-2) var(--s-4)' }, [
      el('summary', { text: t('common.details') }),
      el('code.details-body', { text: t('dupe.hash', { hash: g.hash }) }),
    ])
  );
  return karte;
}

let leiste = null;

function aktionsLeiste() {
  leiste = el('div.card.row', { style: 'position:sticky;bottom:0;margin-top:12px' });
  aktualisiereLeiste();
  return leiste;
}

function aktualisiereLeiste() {
  if (!leiste) return;
  clear(leiste);
  const n = entfernen.size;
  leiste.append(
    el('span.small.muted', {
      text: n ? t('dupe.selected_count', { count: n }) : t('dupe.nothing_selected'),
    }),
    el('span', { style: 'flex:1' }),
    btn({
      icon: 'trash',
      text: t('dupe.trash_selected'),
      kind: 'danger',
      disabled: !n,
      onClick: loeschen,
    })
  );
}

async function loeschen() {
  const pfade = [...entfernen];
  const endgueltig = store.config?.delete_mode === 'permanent';
  const ok = await dialog({
    title: endgueltig ? t('triage.permanent_title') : t('files.delete_recycle_title'),
    body: el('div', {}, [
      el('p', {
        text: endgueltig
          ? t('dupe.delete_permanent_body', { count: pfade.length })
          : t('dupe.delete_recycle_body', { count: pfade.length }),
      }),
      el('div.list-preview', {}, pfade.slice(0, 50).map((p) => el('div', { text: p }))),
    ]),
    buttons: [
      { label: t('common.cancel'), value: false },
      { label: endgueltig ? t('triage.permanent_confirm') : t('files.to_trash'), value: true, kind: 'danger' },
    ],
  });
  if (!ok) return;

  try {
    const r = await api.deleteMany(pfade, endgueltig ? true : undefined);
    toast(
      t(r.failed ? 'dupe.removed_failed' : 'dupe.removed', { done: r.done, failed: r.failed }),
      r.failed ? 'error' : 'success',
      r.done && !endgueltig
        ? {
            label: t('common.undo'),
            onClick: async () => {
              const e = await api.undoBatch(r.batch_id);
              toast(t('dupe.restored', { count: e.filter((x) => x.ok).length }));
              neuZeichnen();
            },
          }
        : null
    );
    entfernen = new Set();
    neuZeichnen();
  } catch (e) {
    toast(e?.title || t('common.generic_failed'), 'error');
  }
}
