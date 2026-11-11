// Startbildschirm.
//
// Zwei Gesichter, je nachdem was schon da ist:
//
//   * Erster Start — eine große Frage („Welcher Ordner?") und Antworten zum
//     Anklicken. Keine Kennzahlen, kein Dashboard, keine Einrichtung.
//   * Danach — der zuletzt bearbeitete Ordner zum Weitermachen, die zuletzt
//     verwendeten Ordner, und die Werkzeuge als ruhige zweite Reihe.

import * as api from '../lib/api.js';
import { btn, dialog, el, iconBtn, toast } from '../lib/dom.js';
import { icon, iconFuerOrdner } from '../lib/icons.js';
import { bytes, datumZeit, kurzerPfad } from '../lib/format.js';
import { gehZu, ladeConfig, ordnerUebernehmen, ordnerWaehlen, store } from '../app.js';
import { t } from '../lib/i18n.js';

export async function render() {
  const cfg = store.config ?? {};
  const hatVerlauf = (cfg.recent_roots?.length ?? 0) > 0 || !!store.root;
  return hatVerlauf ? await wiederkehrend() : await ersterStart();
}

// ---------------------------------------------------------------------------
// Erster Start
// ---------------------------------------------------------------------------

async function ersterStart() {
  const bekannte = await api.knownFolders().catch(() => []);

  const wurzel = el('div.hero', {}, [
    el('div.hero-mark', {}, [icon('inbox', { size: 30, strich: 1.6 })]),
    el('h1', { text: t('app.name') }),
    el('p.claim', { text: t('app.tagline') }),

    el('div.picks', {}, [
      // Die Hauptaktion steht vorn und sieht auch so aus.
      el(
        'button.pick',
        { type: 'button', onClick: ordnerWaehlen, style: 'border-color:var(--accent)' },
        [
          el('span.pick-icon', { style: 'background:var(--accent);color:var(--accent-text)' }, [
            icon('folderOpen', { size: 20 }),
          ]),
          el('span.pick-body', {}, [
            el('span.pick-title', { text: t('home.choose_folder') }),
            el('span.pick-sub', { text: t('home.drag_hint') }),
          ]),
          icon('chevronRight', { size: 16 }),
        ]
      ),
      ...bekannte.map(ordnerKachel),
    ]),

    el('div.hero-foot', {}, [
      el('span.assurance', {}, [
        icon('shield', { size: 14 }),
        el('span', { text: t('home.assurance.trash') }),
      ]),
      el('span.assurance', {}, [
        icon('undo', { size: 14 }),
        el('span', { text: t('home.assurance.undo') }),
      ]),
      el('span.assurance', {}, [
        icon('lock', { size: 14 }),
        el('span', { text: t('home.assurance.local') }),
      ]),
    ]),
  ]);

  groessenNachtragen(wurzel);
  return wurzel;
}

/**
 * Traegt Dateizahl und Gesamtgroesse nach, sobald sie da sind.
 *
 * Bewusst *nach* dem Zeichnen: Die Startseite darf nicht darauf warten. Und
 * bewusst nur aus dem Index — es wird nichts gescannt und nichts von der
 * Platte gelesen, nur um eine Zahl anzuzeigen. Kennt der Index einen Ordner
 * noch nicht, bleibt die Zeile leer statt eine Null zu behaupten.
 */
async function groessenNachtragen(wurzel) {
  const felder = [...wurzel.querySelectorAll('[data-groesse]')];
  if (!felder.length) return;
  let zahlen;
  try {
    zahlen = await api.folderSizes(felder.map((f) => f.dataset.groesse));
  } catch {
    return;
  }
  const nach = new Map(zahlen.map((z) => [z.path.toLowerCase(), z]));
  for (const feld of felder) {
    const z = nach.get(feld.dataset.groesse.toLowerCase());
    if (!z?.known) continue;
    feld.textContent = t('home.folder_stats', {
      count: t('home.file_count', { count: z.files }),
      size: bytes(z.total_size),
    });
  }
}

/**
 * Anzeigename eines bekannten Windows-Ordners.
 *
 * Der Kern liefert `key` ("downloads", "pictures", ...) und `label`. Der
 * Schluessel wird uebersetzt, das Label ist nur die Rueckfallebene. Es waere
 * naheliegend gewesen, einfach `label` anzuzeigen - dann stuende in einer
 * englischen Oberflaeche aber "Dokumente".
 */
function ordnerName(o) {
  const schluessel = `folder.${o.key}`;
  const uebersetzt = t(schluessel);
  return uebersetzt === schluessel ? o.label : uebersetzt;
}

function ordnerKachel(o) {
  return el('button.pick', { type: 'button', onClick: () => ordnerUebernehmen(o.path) }, [
    el('span.pick-icon', {}, [icon(iconFuerOrdner(o.key), { size: 19 })]),
    el('span.pick-body', {}, [
      el('span.pick-title', { text: ordnerName(o) }),
      el('span.pick-sub', { title: o.path, text: o.path }),
      // Bleibt leer, bis die Zahlen aus dem Index da sind - und bleibt leer,
      // wenn der Index diesen Ordner nicht kennt.
      el('span.pick-meta', { 'data-groesse': o.path }),
    ]),
    icon('chevronRight', { size: 16 }),
  ]);
}

// ---------------------------------------------------------------------------
// Wiederkehrender Start
// ---------------------------------------------------------------------------

async function wiederkehrend() {
  const [ueberblick, recent, bekannte, sitzungen] = await Promise.all([
    store.root ? api.overview(store.root).catch(() => null) : Promise.resolve(null),
    api.recentRoots().catch(() => []),
    api.knownFolders().catch(() => []),
    api.listSessions().catch(() => []),
  ]);

  store.offen = ueberblick?.pending ?? 0;

  const weitere = bekannte.filter(
    (b) => !recent.some((r) => r.path.toLowerCase() === b.path.toLowerCase())
  );

  const wurzel = el('div.view-narrow', {}, [
    // Weitermachen, wo es aufgehört hat — passiv angeboten, nichts läuft von selbst.
    store.root && ueberblick
      ? el('div.card', { style: 'display:flex;gap:var(--s-4);align-items:center' }, [
          el('span.pick-icon', { style: 'background:var(--accent-soft);color:var(--accent)' }, [
            icon('inbox', { size: 20 }),
          ]),
          el('div.grow', {}, [
            el('div', {
              style: 'font-weight:600;font-size:var(--t-md)',
              text: store.rootName || kurzerPfad(store.root, 40),
            }),
            el('div.small.muted', {
              text: ueberblick.pending
                ? t('home.pending', { count: ueberblick.pending })
                : t('home.done'),
            }),
          ]),
          ueberblick.pending
            ? btn({
                icon: 'play',
                text: t('home.start_triage'),
                kind: 'primary',
                size: 'large',
                onClick: () => gehZu('triage'),
              })
            : btn({ icon: 'folderOpen', text: t('home.other_folder'), onClick: ordnerWaehlen }),
        ])
      : el('div.card', { style: 'display:flex;gap:var(--s-4);align-items:center' }, [
          el('div.grow', {}, [
            el('div', { style: 'font-weight:600', text: t('home.no_folder') }),
            el('div.small.muted', { text: t('home.no_folder_hint') }),
          ]),
          btn({
            icon: 'folderOpen',
            text: t('home.choose_folder_short'),
            kind: 'primary',
            size: 'large',
            onClick: ordnerWaehlen,
          }),
        ]),

    // Zuletzt verwendet.
    recent.length
      ? el('div', {}, [
          el('div.section-title', {}, [icon('history', { size: 14 }), el('span', { text: t('home.recent') })]),
          el('div.picks', { style: 'display:flex;flex-direction:column;gap:var(--s-2)' },
            recent.map(recentZeile)),
        ])
      : null,

    weitere.length
      ? el('div', {}, [
          el('div.section-title', {}, [
            icon('folder', { size: 14 }),
            el('span', { text: t('home.known_folders') }),
          ]),
          el('div.picks', { style: 'display:flex;flex-direction:column;gap:var(--s-2)' },
            weitere.map(ordnerKachel)),
        ])
      : null,

    // Werkzeuge — ruhige zweite Reihe, kein Kennzahlenteppich.
    store.root
      ? el('div', {}, [
          el('div.section-title', {}, [
            icon('drive', { size: 14 }),
            el('span', { text: t('home.tools') }),
          ]),
          el('div.tiles', {}, [
            werkzeug('copy', t('home.tool.duplicates'), t('home.tool.duplicates_sub'), () => gehZu('duplicates')),
            werkzeug('drive', t('home.tool.large'), t('home.tool.large_sub'), () => gehZu('large')),
            werkzeug('clock', t('home.tool.old'), t('home.tool.old_sub'), () => gehZu('old')),
          ]),
        ])
      : null,

    // Sitzung zurücknehmen — nur wenn es wirklich etwas zurückzunehmen gibt.
    sitzungen.some((s) => s.undoable > 0)
      ? el('div', {}, [
          el('div.section-title', {}, [
            icon('undo', { size: 14 }),
            el('span', { text: t('home.sessions') }),
          ]),
          el('div.card.flat', {}, sitzungen.filter((s) => s.undoable > 0).slice(0, 3).map(sitzungsZeile)),
        ])
      : null,

    el('div.hero-foot', { style: 'justify-content:flex-start;margin-top:var(--s-8)' }, [
      el('span.assurance', {}, [
        icon('lock', { size: 14 }),
        el('span', { text: t('home.assurance.privacy') }),
      ]),
    ]),
  ]);

  groessenNachtragen(wurzel);
  return wurzel;
}

function werkzeug(name, titel, unter, onClick) {
  return el('button.pick', { type: 'button', onClick }, [
    el('span.pick-icon', {}, [icon(name, { size: 19 })]),
    el('span.pick-body', {}, [
      el('span.pick-title', { text: titel }),
      el('span.pick-sub', { text: unter }),
    ]),
  ]);
}

function recentZeile(r) {
  const zeile = el('button.pick', {
    type: 'button',
    disabled: !r.available,
    title: r.available ? r.path : t('home.unavailable_title', { path: r.path }),
    onClick: () => ordnerUebernehmen(r.path),
  }, [
    el('span.pick-icon', {}, [icon(r.available ? 'folder' : 'alertTriangle', { size: 19 })]),
    el('span.pick-body', {}, [
      el('span.pick-title', { text: r.name }),
      el('span.pick-sub', {
        dir: 'ltr',
        text: r.available ? r.path : t('home.unavailable'),
      }),
      el('span.pick-meta', { 'data-groesse': r.path }),
    ]),
    r.pending ? el('span.pill.accent', { text: t('home.open_count', { count: r.pending }) }) : null,
  ]);

  // Entfernen liegt daneben, nicht im Knopf — sonst löscht ein Fehlklick den
  // Eintrag statt ihn zu öffnen.
  return el('div.recent-row', {}, [
    zeile,
    iconBtn('x', t('home.forget', { name: r.name }), {
      kind: 'ghost',
      onClick: async () => {
        await api.forgetRecent(r.path);
        await ladeConfig();
        gehZu('home');
      },
    }),
  ]);
}

function sitzungsZeile(s) {
  const teile = [];
  if (s.moved) teile.push(t('home.session_moved', { count: s.moved }));
  if (s.renamed) teile.push(t('home.session_renamed', { count: s.renamed }));
  if (s.trashed) teile.push(t('home.session_trashed', { count: s.trashed }));
  if (s.deleted) teile.push(t('home.session_deleted', { count: s.deleted }));

  return el('div.row', { style: 'padding:var(--s-2) 0;gap:var(--s-3)' }, [
    el('div.grow', {}, [
      el('div.small', { text: teile.join(' · ') || t('home.session_none') }),
      el('div.xs.dim', { text: t('home.session_range', {
      from: datumZeit(s.started_at),
      to: datumZeit(s.ended_at),
    }) }),
    ]),
    el('span.pill', { text: t('home.session_undoable', { count: s.undoable }) }),
    btn({ icon: 'undo', text: t('home.session_undo'), size: 'small', onClick: () => sitzungZuruecknehmen(s) }),
  ]);
}

/** Zeigt erst den Dry Run, danach erst wird etwas bewegt. */
async function sitzungZuruecknehmen(s) {
  let plan;
  try {
    plan = await api.planSessionUndo(s.session_id);
  } catch (e) {
    toast(e?.title ?? t('home.undo_check_failed'), 'error');
    return;
  }

  const konflikte = plan.items.filter((i) => i.verdict !== 'safe');
  const koerper = el('div', {}, [
    el('p', {
      text: t('home.undo_plan_summary', {
        safe: plan.safe,
        total: plan.total,
        conflicted: plan.conflicted,
        manual: plan.manual,
      }),
    }),
    konflikte.length
      ? el('div', {}, [
          el('p.small.muted', {
            style: 'margin-bottom:var(--s-1)',
            text: t('home.undo_skipped'),
          }),
          el(
            'div.list-preview',
            {},
            konflikte
              .slice(0, 30)
              .map((i) => el('div', {
                text: `${i.original_name} — ${i.reason ? t(`undo.reason.${i.reason}`) : t('common.conflict')}`,
              }))
          ),
        ])
      : null,
  ]);

  const ok = await dialog({
    title: t('home.undo_session_title'),
    body: koerper,
    buttons: [
      { label: t('common.cancel'), value: false },
      plan.safe ? { label: t('home.undo_session_confirm', { count: plan.safe }), value: true, kind: 'primary' } : null,
    ],
  });
  if (!ok) return;

  try {
    const ergebnisse = await api.undoSession(s.session_id);
    const gut = ergebnisse.filter((r) => r.ok).length;
    toast(t('home.undo_result', { done: gut, total: ergebnisse.length }), gut ? 'success' : 'info');
    gehZu('home');
  } catch (e) {
    toast(e?.title ?? t('home.undo_failed'), 'error');
  }
}
