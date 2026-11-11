// Regeln und Vorschläge.
//
// Die wichtigste Aufgabe dieser Seite ist nicht das Bearbeiten von Regeln,
// sondern eine Antwort auf die Frage, die jeder zuerst stellt: „Sortiert das
// jetzt automatisch meine Dateien?" Die Antwort ist Nein, und sie steht oben
// als erster Satz — nicht im Kleingedruckten und nicht in einem Popup.
//
// Nichts hier verschiebt etwas ohne Vorschau und ausdrückliche Bestätigung.

import * as api from '../lib/api.js';
import { btn, clear, dialog, el, fehlerDialog, iconBtn, toast } from '../lib/dom.js';
import { icon } from '../lib/icons.js';
import { bytes, kategorie, kurzerPfad } from '../lib/format.js';
import { neuZeichnen, store } from '../app.js';
import { t } from '../lib/i18n.js';
import { zahl } from '../lib/i18n.js';

// Feld, moegliche Vergleiche, Platzhalter fuer das Wertfeld.
//
// Beschriftungen stehen bewusst NICHT hier: Diese Tabelle wird beim Laden des
// Moduls ausgewertet, also bevor eine Sprache geladen ist. Uebersetzt wird
// erst beim Zeichnen, ueber `feldName` und `opName`.
const FELDER = [
  ['name', ['contains', 'starts_with', 'ends_with', 'eq'], 'rules.placeholder.name'],
  ['ext', ['in'], 'rules.placeholder.ext'],
  ['category', ['in'], 'rules.placeholder.category'],
  ['path', ['contains', 'starts_with', 'ends_with'], 'rules.placeholder.path'],
  ['size', ['gt', 'lt'], ''],
  ['age_days', ['gt', 'lt'], ''],
];

const FELD_SCHLUESSEL = {
  name: 'rules.field.name',
  ext: 'rules.field.ext',
  category: 'rules.field.category',
  path: 'rules.field.path',
  size: 'rules.field.size',
  age_days: 'rules.field.age',
};

const feldName = (f) => t(FELD_SCHLUESSEL[f] ?? f);
const opName = (op) => t(`rules.op.${op}`);

const ZAHLENFELD = (f) => ['size', 'age_days'].includes(f);

export async function render() {
  const [regeln, typVorschlaege, musterVorschlaege] = await Promise.all([
    api.listRules(),
    api.ruleSuggestions().catch(() => []),
    api.patternSuggestions().catch(() => []),
  ]);

  // Die Trefferzahlen kommen nachgelagert. Sie sind nützlich, aber niemand
  // soll auf sie warten, um die Seite zu sehen.
  const nachtragen = async (wurzel) => {
    let zahlen;
    try {
      zahlen = await api.ruleMatchCounts(store.root);
    } catch {
      return;
    }
    const zaehler = new Map(zahlen.map((z) => [z.rule_id, z]));
    for (const feld of wurzel.querySelectorAll('[data-treffer]')) {
      const z = zaehler.get(Number(feld.dataset.treffer));
      if (!z) continue;
      feld.textContent = z.count
        ? z.truncated
          ? t('rules.matches_at_least', { count: z.count })
          : t('rules.matches_here', { count: z.count })
        : t('rules.no_matches_here');
    }
  };

  const vorschlaege = [
    ...musterVorschlaege.map((v) => ({ art: 'muster', v })),
    ...typVorschlaege.map((v) => ({ art: 'typ', v })),
  ];

  const wurzel = el('div.view-narrow', {}, [
    el('div', { style: 'margin-bottom:var(--s-4)' }, [
      el('h2', { style: 'font-size:var(--t-xl)', text: t('rules.title') }),
    ]),

    // Die Kernbotschaft. Sichtbar, kurz, immer da.
    el('div.card.erklaerung', {}, [
      el('div.erklaerung-kopf.mit-icon', {}, [
        icon('shield', { size: 18 }),
        el('strong', { text: t('rules.headline') }),
      ]),
      el('p', { text: t('rules.body') }),
      el('ul.erklaerung-liste', {}, [
        el('li', { text: t('rules.point.which') }),
        el('li', { text: t('rules.point.where') }),
        el('li', { text: t('rules.point.howmany') }),
        el('li', { text: t('rules.point.conflicts') }),
      ]),
      el('p.small.muted', {
        text: t('rules.no_background'),
      }),
    ]),

    // Vorschläge liegen eingeklappt herum und melden sich nie von selbst.
    vorschlaege.length
      ? el('details.details', { style: 'margin:var(--s-5) 0' }, [
          el('summary', { text: t('rules.suggestions', { count: vorschlaege.length }) }),
          el('div', { style: 'padding-top:var(--s-2)' }, [
            el('p.small.muted', {
              style: 'margin:0 0 var(--s-3)',
              text: t('rules.suggestions_hint'),
            }),
            ...vorschlaege.map((e) => (e.art === 'muster' ? musterKarte(e.v) : typKarte(e.v))),
          ]),
        ])
      : null,

    el('div.row', { style: 'margin-bottom:var(--s-3)' }, [
      el('div.grow'),
      btn({
        icon: 'plus',
        text: t('rules.new'),
        kind: 'primary',
        size: 'small',
        onClick: () => bearbeiten(null),
      }),
    ]),

    regeln.length
      ? el('div', {}, regeln.map(regelKarte))
      : el('div.empty', {}, [
          el('div.empty-mark', {}, [icon('rules', { size: 26 })]),
          el('h2', { text: t('rules.empty_title') }),
          el('p', {
            text: t('rules.empty_body'),
          }),
        ]),
  ]);

  if (regeln.length) nachtragen(wurzel);
  return wurzel;
}

// ---------------------------------------------------------------------------
// Vorschläge
// ---------------------------------------------------------------------------

/** Vorschlag aus einem wiederkehrenden Wort im Dateinamen. */
function musterKarte(v) {
  const bedingung = t(v.ext ? 'rules.suggestion_name_ext' : 'rules.suggestion_name', {
    token: v.token,
    ext: v.ext,
  });
  const treffer = el('div.small.muted');
  api
    .patternMatchCount(v, store.root)
    .then((z) => {
      treffer.textContent = z.count
        ? z.truncated
          ? t('rules.matches_at_least', { count: z.count })
          : t('rules.current_matches', { count: z.count })
        : t('rules.no_matches_here');
    })
    .catch(() => {});
  return el('div.card.vorschlag', {}, [
    el('div.vorschlag-regel', {}, [
      el('code', { text: bedingung }),
      el('span.mit-icon', {}, [
        icon('arrowRight', { size: 14 }),
        el('span', { text: kurzerPfad(v.target, 44) }),
      ]),
    ]),
    // Warum? Belege statt Behauptung.
    el('div.small.muted', {
      style: 'margin-top:var(--s-2)',
      text: t('rules.pattern_reason', {
        count: v.count,
        token: v.token,
        target: kurzerPfad(v.target, 30),
      }),
    }),
    v.examples?.length
      ? el('div.small.muted.beispiele', { text: t('rules.examples', { list: v.examples.join(', ') }) })
      : null,
    // Die aktuelle Trefferzahl wird nachgetragen - sie ist nett, aber niemand
    // soll auf sie warten.
    treffer,
    el('div.row', { style: 'margin-top:var(--s-3);gap:var(--s-2)' }, [
      btn({
        icon: 'plus',
        text: t('rules.save_as_rule'),
        kind: 'primary',
        size: 'small',
        onClick: async () => {
          try {
            await api.acceptPattern(v);
            toast(t('rules.saved_passive'), 'success');
            neuZeichnen();
          } catch (e) {
            await fehlerDialog(e);
          }
        },
      }),
      btn({
        text: t('rules.dismiss'),
        kind: 'ghost',
        size: 'small',
        onClick: async () => {
          await api.dismissPattern(v.token, v.target);
          neuZeichnen();
        },
      }),
    ]),
  ]);
}

/** Vorschlag aus Dateityp oder Endung — der ältere Weg, unverändert. */
function typKarte(v) {
  return el('div.card.vorschlag', {}, [
    el('div.vorschlag-regel', {}, [
      el('code', {
        text: v.ext
          ? t('rules.suggestion_ext', { ext: v.ext })
          : t('rules.suggestion_category', { category: kategorie(v.category) }),
      }),
      el('span.mit-icon', {}, [
        icon('arrowRight', { size: 14 }),
        el('span', { text: kurzerPfad(v.target, 44) }),
      ]),
    ]),
    el('div.small.muted', {
      style: 'margin-top:var(--s-2)',
      text: t(v.ext ? 'rules.type_reason_ext' : 'rules.type_reason_category', {
        ext: v.ext,
        category: kategorie(v.category),
        count: v.count,
        target: kurzerPfad(v.target, 30),
      }),
    }),
    el('div.row', { style: 'margin-top:var(--s-3);gap:var(--s-2)' }, [
      btn({
        icon: 'plus',
        text: t('rules.save_as_rule'),
        kind: 'primary',
        size: 'small',
        onClick: async () => {
          try {
            await api.acceptSuggestion(v);
            toast(t('rules.saved_passive'), 'success');
            neuZeichnen();
          } catch (e) {
            await fehlerDialog(e);
          }
        },
      }),
      btn({
        text: t('rules.dismiss'),
        kind: 'ghost',
        size: 'small',
        onClick: async () => {
          await api.dismissSuggestion(v.ext, v.category, v.target);
          neuZeichnen();
        },
      }),
    ]),
  ]);
}

// ---------------------------------------------------------------------------
// Regelkarte
// ---------------------------------------------------------------------------

function regelKarte(r) {
  return el('div.card.regel', {}, [
    el('div.row', { style: 'align-items:flex-start' }, [
      el('div.grow', { style: 'min-width:0' }, [
        el('div.row', { style: 'gap:var(--s-2)' }, [
          el('span', { style: 'font-weight:600', text: r.name }),
          r.enabled
            ? el('span.pill.ok', { text: t('rules.active') })
            : el('span.pill', { text: t('rules.disabled') }),
          r.source === 'suggested' ? el('span.pill', { text: t('rules.from_suggestion') }) : null,
        ]),

        // Was die Regel findet — in derselben Sprache wie im Editor.
        el('div.regel-satz', {}, [
          el('span.regel-wort', { text: t('rules.if') }),
          ...bedingungsZeilen(r),
          el('span.regel-wort.regel-dann', { text: t('rules.then') }),
          el('span.regel-ziel.mit-icon', {}, [
            icon('folder', { size: 14 }),
            el('span', { text: kurzerPfad(r.target, 52) }),
          ]),
        ]),

        el('div.small.muted.regel-status', {}, [
          el('span', { 'data-treffer': String(r.id), text: t('rules.counting') }),
          el('span', { text: ' · ' }),
          el('span', { text: t('rules.only_on_confirm') }),
          r.hits
            ? el('span', { text: ` · ${t('rules.applied', { count: r.hits })}` })
            : null,
        ]),
      ]),

      el('div.regel-aktionen', {}, [
        btn({
          icon: 'eye',
          text: t('rules.preview'),
          size: 'small',
          disabled: !r.enabled,
          title: r.enabled ? null : t('rules.disabled_hint'),
          onClick: () => vorschau(r),
        }),
        iconBtn(
          r.enabled ? 'pause' : 'play',
          t(r.enabled ? 'rules.disable' : 'rules.enable', { name: r.name }),
          {
            kind: 'ghost',
            size: 'small',
            onClick: async () => {
              try {
                await api.saveRule({ ...r, enabled: !r.enabled });
                neuZeichnen();
              } catch (e) {
                await fehlerDialog(e);
              }
            },
          }
        ),
        iconBtn('pencil', t('rules.edit', { name: r.name }), {
          kind: 'ghost',
          size: 'small',
          onClick: () => bearbeiten(r),
        }),
        iconBtn('x', t('rules.remove', { name: r.name }), {
          kind: 'ghost',
          size: 'small',
          onClick: async () => {
            const ok = await dialog({
              title: t('rules.delete_title'),
              body: el('p', {
                text: t('rules.delete_body', { name: r.name }),
              }),
              buttons: [
                { label: t('common.cancel'), value: false },
                { label: t('common.delete'), value: true, kind: 'danger' },
              ],
            });
            if (!ok) return;
            await api.deleteRule(r.id);
            neuZeichnen();
          },
        }),
      ]),
    ]),
  ]);
}

/** Die Bedingungen als lesbare Zeilen, mit UND bzw. ODER dazwischen. */
function bedingungsZeilen(r) {
  const verknuepfung = t(r.match_mode === 'any' ? 'rules.or_short' : 'rules.and_short');
  const out = [];
  r.conditions.forEach((c, i) => {
    if (i > 0) out.push(el('span.regel-wort', { text: verknuepfung }));
    out.push(el('code.regel-bedingung', { text: bedingungsText(c) }));
  });
  return out;
}

function bedingungsText(c) {
  const feld = feldName(c.field);
  const roh = Array.isArray(c.value) ? c.value : [c.value];
  const wert = roh
    .map((v) => (c.field === 'category' ? kategorie(String(v)) : v))
    .map((v) => (c.field === 'ext' ? `.${String(v).replace(/^\./, '')}` : v))
    .join(', ');
  const op = opName(c.op);
  return ZAHLENFELD(c.field) ? `${feld} ${op} ${wert}` : `${feld} ${op} „${wert}"`;
}

// ---------------------------------------------------------------------------
// Vorschau
// ---------------------------------------------------------------------------

async function vorschau(r) {
  let lauf;
  try {
    lauf = await api.dryRun(r.id, store.root);
  } catch (e) {
    await fehlerDialog(e);
    return;
  }

  if (!lauf.count) {
    await dialog({
      title: t('rules.preview_title', { name: r.name }),
      body: el('div', {}, [
        el('p', { text: t('rules.preview_none') }),
        el('p.small.muted', { text: t('rules.preview_nothing_changed') }),
      ]),
      buttons: [{ label: t('common.close'), value: null }],
    });
    return;
  }

  const ok = await dialog({
    title: t('rules.preview_title', { name: r.name }),
    body: el('div.vorschau', {}, [
      // Die Zahlen zuerst, groß und ohne Fließtext drumherum.
      el('div.vorschau-zahlen', {}, [
        kennzahl(zahl(lauf.count), t('rules.preview_files', { count: lauf.count })),
        kennzahl(bytes(lauf.total_size), t('rules.preview_total')),
      ]),
      el('div.vorschau-ziel', {}, [
        el('div.small.muted', { text: t('common.target') }),
        el('div.mit-icon', {}, [icon('folder', { size: 15 }), el('span', { text: lauf.target })]),
      ]),
      el('div.vorschau-lage', {}, [
        el('span.pill.ok', { text: t('rules.preview_safe', { count: lauf.safe }) }),
        lauf.conflicts
          ? el('span.pill.warn', {
              text: t('rules.preview_conflicts', { count: lauf.conflicts }),
            })
          : null,
      ]),
      lauf.conflicts
        ? el('p.small.muted', {
            text: t('rules.preview_conflict_hint'),
          })
        : null,
      el('details.details', {}, [
        el('summary', { text: t('rules.preview_show_all', { count: lauf.count }) }),
        el(
          'div.list-preview',
          {},
          lauf.files.slice(0, 200).map((f) => el('div', { text: f.path }))
        ),
        lauf.count > 200
          ? el('div.small.muted', {
              text: t('common.more_count', { count: lauf.count - 200 }),
            })
          : null,
      ]),
      lauf.truncated
        ? el('p.small', {
            style: 'color:var(--warn)',
            text: t('rules.preview_truncated', { count: lauf.count }),
          })
        : null,
    ]),
    buttons: [
      {
        label: t('rules.preview_execute', { count: lauf.count }),
        value: true,
        kind: 'primary',
      },
      { label: t('common.cancel'), value: false },
    ],
  });
  if (!ok) return;

  try {
    // Die bestätigte Anzahl geht mit: Weicht der tatsächliche Umfang stark
    // davon ab, führt das Backend nichts aus.
    const erg = await api.applyRule(r.id, store.root, 'rename', lauf.count);
    toast(
      t(erg.failed ? 'rules.preview_applied_failed' : 'rules.preview_applied', {
        done: erg.done,
        failed: erg.failed,
      }),
      erg.failed ? 'error' : 'success',
      erg.done
        ? {
            label: t('common.undo'),
            onClick: async () => {
              const e = await api.undoBatch(erg.batch_id);
              toast(t('rules.undo_result', { count: e.filter((x) => x.ok).length }));
              neuZeichnen();
            },
          }
        : null
    );
    neuZeichnen();
  } catch (e) {
    await fehlerDialog(e);
  }
}

function kennzahl(wert, label) {
  return el('div.kennzahl', {}, [
    el('div.kennzahl-wert', { text: wert }),
    el('div.kennzahl-label', { text: label }),
  ]);
}

// ---------------------------------------------------------------------------
// Editor
// ---------------------------------------------------------------------------

async function bearbeiten(vorhanden) {
  const regel = vorhanden
    ? structuredClone(vorhanden)
    : {
        id: null,
        name: '',
        enabled: true,
        priority: 100,
        conditions: [{ field: 'name', op: 'contains', value: [] }],
        match_mode: 'all',
        target: '',
        source: 'user',
        created_at: 0,
        hits: 0,
      };

  const nameFeld = el('input', {
    type: 'text',
    value: regel.name,
    placeholder: t('rules.name_placeholder'),
  });
  const zielFeld = el('input', {
    type: 'text',
    value: regel.target,
    placeholder: t('rules.target_placeholder'),
    style: 'flex:1',
  });
  const bedingungen = el('div.editor-bedingungen');

  const zeichneBedingungen = () => {
    clear(bedingungen);
    regel.conditions.forEach((c, i) => {
      if (i > 0) {
        // Die Verknüpfung steht zwischen den Bedingungen, wo man sie liest —
        // nicht als Einstellung irgendwo daneben.
        bedingungen.append(
          el('div.editor-verknuepfung', {}, [
            el(
              'select',
              {
                'aria-label': t('rules.aria.join'),
                onChange: (e) => {
                  regel.match_mode = e.target.value;
                  zeichneBedingungen();
                },
              },
              [
                el('option', {
                  value: 'all',
                  text: t('rules.and'),
                  selected: regel.match_mode !== 'any',
                }),
                el('option', {
                  value: 'any',
                  text: t('rules.or'),
                  selected: regel.match_mode === 'any',
                }),
              ]
            ),
          ])
        );
      }

      const eintrag = FELDER.find((f) => f[0] === c.field) ?? FELDER[0];
      const feldWahl = el(
        'select',
        {
          'aria-label': t('rules.aria.field'),
          onChange: (e) => {
            c.field = e.target.value;
            c.op = FELDER.find((f) => f[0] === c.field)[1][0];
            c.value = ZAHLENFELD(c.field) ? 0 : [];
            zeichneBedingungen();
          },
        },
        FELDER.map(([k]) => el('option', { value: k, text: feldName(k), selected: k === c.field }))
      );
      const opWahl = el(
        'select',
        { 'aria-label': t('rules.aria.op'), onChange: (e) => (c.op = e.target.value) },
        eintrag[1].map((op) =>
          el('option', { value: op, text: opName(op), selected: op === c.op })
        )
      );
      const wertFeld = el('input', {
        type: ZAHLENFELD(c.field) ? 'number' : 'text',
        'aria-label': t('rules.aria.value'),
        value: Array.isArray(c.value) ? c.value.join(', ') : c.value,
        placeholder: eintrag[2] ? t(eintrag[2]) : '',
        style: 'flex:1;min-width:8ch',
        onChange: (e) => {
          c.value = ZAHLENFELD(c.field)
            ? Number(e.target.value) || 0
            : e.target.value
                .split(',')
                .map((s) => s.trim())
                .filter(Boolean);
        },
      });

      bedingungen.append(
        el('div.editor-zeile', {}, [
          feldWahl,
          opWahl,
          wertFeld,
          regel.conditions.length > 1
            ? iconBtn('x', t('rules.remove_condition'), {
                kind: 'ghost',
                size: 'small',
                onClick: () => {
                  regel.conditions.splice(i, 1);
                  zeichneBedingungen();
                },
              })
            : null,
        ])
      );
    });

    bedingungen.append(
      btn({
        icon: 'plus',
        text: t('rules.add_condition'),
        size: 'small',
        onClick: () => {
          regel.conditions.push({ field: 'ext', op: 'in', value: [] });
          zeichneBedingungen();
        },
      })
    );
  };
  zeichneBedingungen();

  const ok = await dialog({
    title: t(vorhanden ? 'rules.edit_title' : 'rules.new_title'),
    body: el('div.editor', {}, [
      el('div.editor-block', {}, [el('div.editor-wort', { text: t('rules.if') }), bedingungen]),
      el('div.editor-block', {}, [
        el('div.editor-wort', { text: t('rules.then') }),
        el('div.row', {}, [
          zielFeld,
          btn({
            icon: 'folderOpen',
            text: t('common.choose'),
            size: 'small',
            onClick: async () => {
              const p = await api.pickFolder(t('rules.pick_target'));
              if (p) zielFeld.value = p;
            },
          }),
        ]),
      ]),
      el('label.field', { style: 'margin-top:var(--s-4)' }, [
        el('span', { text: t('rules.name_label') }),
        nameFeld,
      ]),
      el('div.editor-hinweis.mit-icon', {}, [
        icon('shield', { size: 15 }),
        el('span', {
          text: t('rules.editor_hint'),
        }),
      ]),
    ]),
    buttons: [
      { label: t('common.save'), value: true, kind: 'primary' },
      { label: t('common.cancel'), value: false },
    ],
  });
  if (!ok) return;

  regel.name = nameFeld.value.trim();
  regel.target = zielFeld.value.trim();
  try {
    await api.saveRule(regel);
    toast(t('rules.saved'), 'success');
    neuZeichnen();
  } catch (e) {
    await fehlerDialog(e);
  }
}
