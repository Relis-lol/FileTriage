// Einstellungen in zwei Ebenen.
//
// „Allgemein" enthält, was jemand nach einer Woche vielleicht ändern will.
// „Erweitert" enthält, was man nur ändert, wenn man weiß warum — Index,
// Überwachung, endgültiges Löschen. Diese Trennung ist keine Kosmetik: Eine
// gefährliche Option, die zwischen Erscheinungsbild und Sortierung steht, wird
// irgendwann versehentlich umgelegt.

import * as api from '../lib/api.js';
import { btn, clear, dialog, el, iconBtn, toast } from '../lib/dom.js';
import { icon } from '../lib/icons.js';
import { taste } from '../lib/format.js';
import { neuZeichnen, setzeSprache, speichereConfig, store } from '../app.js';
import { SPRACHEN, t } from '../lib/i18n.js';

// Schluessel, nicht Texte: Diese Tabelle steht auf Modulebene und wuerde
// sonst uebersetzt, bevor die Sprache ueberhaupt geladen ist. Uebersetzt wird
// beim Zeichnen.
const AKTIONS_NAMEN = {
  delete: 'hotkey.delete',
  skip: 'triage.skip',
  keep: 'hotkey.keep',
  open: 'common.open',
  explorer: 'hotkey.explorer',
  rename: 'hotkey.rename',
  undo: 'hotkey.undo',
  next: 'hotkey.next',
  prev: 'hotkey.prev',
};

let ebene = 'allgemein';

export async function render() {
  const cfg = structuredClone(store.config);

  const sichern = async () => {
    try {
      await speichereConfig(cfg);
    } catch (e) {
      toast(e?.title ?? t('settings.save_failed'), 'error');
    }
  };

  const inhalt = el('div');
  const zeichne = () => {
    clear(inhalt);
    inhalt.append(ebene === 'allgemein' ? allgemein(cfg, sichern) : erweitert(cfg, sichern));
  };

  const reiter = el('div.tabs', { role: 'tablist' }, [
    el('button', {
      class: `tab${ebene === 'allgemein' ? ' active' : ''}`,
      type: 'button',
      role: 'tab',
      'aria-selected': String(ebene === 'allgemein'),
      text: t('settings.general'),
      onClick: () => {
        ebene = 'allgemein';
        neuZeichnen();
      },
    }),
    el('button', {
      class: `tab${ebene === 'erweitert' ? ' active' : ''}`,
      type: 'button',
      role: 'tab',
      'aria-selected': String(ebene === 'erweitert'),
      text: t('settings.advanced'),
      onClick: () => {
        ebene = 'erweitert';
        neuZeichnen();
      },
    }),
  ]);

  zeichne();
  return el('div.view-narrow', {}, [reiter, inhalt]);
}

// ---------------------------------------------------------------------------
// Allgemein
// ---------------------------------------------------------------------------

function allgemein(cfg, sichern) {
  return el('div', {}, [
    // Zuerst: Wer die Oberflaeche nicht lesen kann, muss die Sprachwahl
    // trotzdem finden - deshalb steht sie oben und nicht unter "Erweitert".
    abschnitt('info', t('settings.language'), t('settings.language_hint'), [
      wahlZeile(
        t('settings.language'),
        [['', t('settings.language_system')], ...Object.entries(SPRACHEN)],
        cfg.language ?? '',
        (wert) => setzeSprache(wert || null)
      ),
    ]),

    abschnitt('folder', t('settings.targets'), t('settings.targets_sub'), [
      zielListe(cfg, sichern),
    ]),

    abschnitt('eye', t('settings.appearance'), null, [
      wahlZeile(
        t('settings.theme'),
        [
          ['system', t('settings.theme_system')],
          ['light', t('settings.theme_light')],
          ['dark', t('settings.theme_dark')],
        ],
        cfg.theme,
        (wert) => {
          cfg.theme = wert;
          sichern().then(() => neuZeichnen());
        }
      ),
      schalter(
        t('settings.previews'),
        t('settings.previews_sub'),
        cfg.previews_enabled,
        (v) => {
          cfg.previews_enabled = v;
          sichern();
        }
      ),
    ]),

    abschnitt('rules', t('settings.suggestions'), null, [
      wahlZeile(
        t('settings.suggestions_mode'),
        [
          ['aus', t('settings.off')],
          ['passiv', t('settings.passive')],
        ],
        cfg.suggestions_enabled ? 'passiv' : 'aus',
        (wert) => {
          cfg.suggestions_enabled = wert === 'passiv';
          sichern();
        }
      ),
      el('p.small.muted', {
        style: 'margin:var(--s-2) 0 0',
        text:
          t('settings.suggestions_explain'),
      }),
    ]),

    abschnitt('drive', t('settings.views'), null, [
      el('div.row', { style: 'gap:var(--s-4)' }, [
        zahlFeld(t('settings.large_threshold'), cfg.large_file_mb, 1, (v) => {
          cfg.large_file_mb = v;
          sichern();
        }),
        zahlFeld(t('settings.old_threshold'), cfg.old_file_days, 1, (v) => {
          cfg.old_file_days = v;
          sichern();
        }),
      ]),
    ]),

    abschnitt('inbox', t('settings.hotkeys'), t('settings.hotkeys_sub'), [
      hotkeyListe(cfg, sichern),
    ]),
  ]);
}

// ---------------------------------------------------------------------------
// Erweitert
// ---------------------------------------------------------------------------

function erweitert(cfg, sichern) {
  const endgueltig = cfg.delete_mode === 'permanent';

  return el('div', {}, [
    abschnitt('search', t('settings.scan'), null, [
      schalter(t('settings.recursive'), null, cfg.scan.recursive, (v) => {
        cfg.scan.recursive = v;
        sichern();
      }),
      schalter(
        t('settings.hidden'),
        t('settings.hidden_sub'),
        cfg.scan.include_hidden,
        (v) => {
          cfg.scan.include_hidden = v;
          sichern();
        }
      ),
      el('p.small.muted', {
        style: 'margin-top:var(--s-2)',
        text: t('settings.skipped_dirs', { list: cfg.scan.skip_dirs.join(', ') }),
      }),
    ]),

    // Die gefährlichste Einstellung des Programms, entsprechend gekennzeichnet.
    abschnitt('alertTriangle', t('settings.delete_behaviour'), null, [
      el('div.card.flat', { style: endgueltig ? 'border-color:var(--danger)' : '' }, [
        el('div.row', {}, [
          el('span.pill', { class: endgueltig ? 'pill danger' : 'pill ok' }, [
            icon(endgueltig ? 'alertTriangle' : 'shield', { size: 13 }),
            el('span', { text: endgueltig ? t('settings.permanent') : t('settings.recycle_bin') }),
          ]),
          el('span.small.muted', {
            text: endgueltig
              ? t('settings.permanent_sub')
              : t('settings.recycle_bin_sub'),
          }),
        ]),
        el('div.row', { style: 'margin-top:var(--s-3)' }, [
          endgueltig
            ? btn({
                icon: 'shield',
                text: t('settings.back_to_trash'),
                kind: 'primary',
                onClick: () => {
                  cfg.delete_mode = 'recycle';
                  cfg.permanent_delete_confirmed_at = null;
                  sichern().then(() => neuZeichnen());
                },
              })
            : btn({
                icon: 'alertTriangle',
                text: t('settings.enable_permanent'),
                kind: 'danger',
                onClick: () => endgueltigEinschalten(cfg, sichern),
              }),
        ]),
      ]),
    ]),

    abschnitt('drive', t('settings.index_data'), null, [
      el('dl.meta', {}, [
        el('dt', { text: t('settings.index') }),
        el('dd', { text: store.info?.db_path ?? '–' }),
        el('dt', { text: t('settings.logs') }),
        el('dd', { text: store.info?.log_dir ?? '–' }),
        el('dt', { text: t('settings.version') }),
        el('dd', { text: store.info?.version ?? '–' }),
      ]),
      el('p.small.muted', {
        style: 'margin-top:var(--s-3)',
        text:
          t('settings.index_explain'),
      }),
    ]),

    abschnitt('lock', t('settings.privacy'), null, [
      el('p.small', {
        text:
          t('settings.privacy_explain'),
      }),
    ]),

    store.lizenz?.channel === 'community'
      ? abschnitt('info', t('community.title'), null, [
          el('p.small', { text: t('community.license') }),
          el('p.small', { text: t('community.commercial') }),
          el('a.btn.ghost.small', {
            href: 'mailto:kontakt@saveroq.com',
            text: 'kontakt@saveroq.com',
          }),
        ])
      : null,
  ]);
}

async function endgueltigEinschalten(cfg, sichern) {
  const ok = await dialog({
    title: t('settings.enable_permanent_title'),
    body: el('div', {}, [
      el('p', {
        text:
          t('settings.permanent_explain'),
      }),
      el('p.small.muted', {
        text: t('settings.enable_permanent_body'),
      }),
    ]),
    buttons: [
      { label: t('common.cancel'), value: false },
      { label: t('settings.enable_anyway'), value: true, kind: 'danger' },
    ],
  });
  if (!ok) return;
  cfg.delete_mode = 'permanent';
  cfg.permanent_delete_confirmed_at = Math.floor(Date.now() / 1000);
  await sichern();
  neuZeichnen();
}

// ---------------------------------------------------------------------------
// Bausteine
// ---------------------------------------------------------------------------

function abschnitt(iconName, titel, unter, kinder) {
  return el('div', { style: 'margin-bottom:var(--s-6)' }, [
    el('div.section-title', { style: 'margin-top:0' }, [
      icon(iconName, { size: 14 }),
      el('span', { text: titel }),
    ]),
    el('div.card', {}, [
      unter ? el('p.small.muted', { text: unter }) : null,
      ...kinder.filter(Boolean),
    ]),
  ]);
}

function zielListe(cfg, sichern) {
  const behaelter = el('div');

  const zeichne = () => {
    clear(behaelter);
    for (const ziel of cfg.targets) {
      behaelter.append(
        el('div.target-row', {}, [
          el('kbd', { text: ziel.key }),
          el('input', {
            type: 'text',
            value: ziel.label,
            'aria-label': t('settings.target_name'),
            onChange: (e) => {
              ziel.label = e.target.value;
              sichern();
            },
          }),
          el('input', {
            type: 'text',
            value: ziel.path,
            'aria-label': t('settings.target_path'),
            title: ziel.path,
            onChange: (e) => {
              ziel.path = e.target.value;
              sichern();
            },
          }),
          el('div.row', { style: 'flex-wrap:nowrap' }, [
            iconBtn('folderOpen', t('settings.pick_for_target', { name: ziel.label }), {
              size: 'small',
              onClick: async () => {
                const p = await api.pickFolder(t('settings.target_picker', { name: ziel.label }));
                if (!p) return;
                ziel.path = p;
                await sichern();
                zeichne();
              },
            }),
            iconBtn('x', t('settings.remove_target', { name: ziel.label }), {
              kind: 'ghost',
              size: 'small',
              onClick: async () => {
                cfg.targets = cfg.targets.filter((t) => t !== ziel);
                await sichern();
                zeichne();
              },
            }),
          ]),
        ])
      );
    }

    if (!cfg.targets.length) {
      behaelter.append(
        el('p.small.muted', {
          text: t('settings.no_targets'),
        })
      );
    }

    if (cfg.targets.length < 9) {
      behaelter.append(
        btn({
          icon: 'folderPlus',
          text: t('settings.add_target'),
          size: 'small',
          onClick: async () => {
            const belegt = new Set(cfg.targets.map((t) => t.key));
            const frei = ['1', '2', '3', '4', '5', '6', '7', '8', '9'].find((k) => !belegt.has(k));
            const pfad = await api.pickFolder(t('settings.pick_target'));
            if (!pfad) return;
            cfg.targets.push({ key: frei, label: pfad.split('\\').pop() || pfad, path: pfad });
            await sichern();
            zeichne();
          },
        })
      );
    }
  };

  zeichne();
  return behaelter;
}

function hotkeyListe(cfg, sichern) {
  const gitter = el('div.hotkey-table');
  for (const [aktion, schluessel] of Object.entries(AKTIONS_NAMEN)) {
    const label = t(schluessel);
    const knopf = el('button.btn.small', {
      type: 'button',
      'aria-label': t('settings.change_key', {
        action: label,
        key: taste(cfg.hotkeys[aktion]),
      }),
    }, [el('kbd', { text: taste(cfg.hotkeys[aktion]) })]);

    knopf.addEventListener('click', () => {
      clear(knopf);
      knopf.append(el('span.small', { text: t('settings.press_key') }));
      const einmal = (event) => {
        event.preventDefault();
        event.stopPropagation();
        document.removeEventListener('keydown', einmal, true);
        clear(knopf);
        if (event.key === 'Escape') {
          knopf.append(el('kbd', { text: taste(cfg.hotkeys[aktion]) }));
          return;
        }
        cfg.hotkeys[aktion] = event.key;
        knopf.append(el('kbd', { text: taste(event.key) }));
        sichern();
      };
      document.addEventListener('keydown', einmal, true);
    });

    gitter.append(el('span', { text: label }), knopf);
  }
  gitter.append(
    el('span.small.muted', { text: t('settings.targets_fixed') }),
    el('span')
  );
  return gitter;
}

function wahlZeile(label, optionen, wert, onChange) {
  return el('label.field', { style: 'margin-bottom:var(--s-2)' }, [
    el('span', { text: label }),
    el(
      'select',
      { onChange: (e) => onChange(e.target.value) },
      optionen.map(([v, t]) => el('option', { value: v, text: t, selected: v === wert }))
    ),
  ]);
}

function zahlFeld(label, wert, min, onChange) {
  return el('label.field', {}, [
    el('span', { text: label }),
    el('input', {
      type: 'number',
      value: wert,
      min,
      onChange: (e) => onChange(Math.max(min, Number(e.target.value) || min)),
    }),
  ]);
}

function schalter(label, unter, wert, onChange) {
  return el('label.switch-row', {}, [
    el('input', { type: 'checkbox', checked: wert, onChange: (e) => onChange(e.target.checked) }),
    el('span.label', {}, [
      el('b', { text: label }),
      unter ? el('span', { text: unter }) : null,
    ]),
  ]);
}
