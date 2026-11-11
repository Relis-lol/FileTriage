// Wiederherstellung nach einem Absturz.
//
// Diese Ansicht erscheint beim Start, wenn unfertige Operationen gefunden
// wurden. Sie entscheidet nichts selbst - sie erklärt, was gefunden wurde, und
// lässt den Nutzer wählen.

import * as api from '../lib/api.js';
import { btn, clear, dialog, el, toast } from '../lib/dom.js';
import { icon } from '../lib/icons.js';
import { bytes, datumZeit, kurzerPfad } from '../lib/format.js';
import { neuZeichnen } from '../app.js';
import { t } from '../lib/i18n.js';

const SCHWERE = {
  kritisch: { klasse: 'danger', text: t('recovery.critical') },
  achtung: { klasse: 'warn', text: t('recovery.check') },
  hinweis: { klasse: '', text: t('recovery.notice') },
  info: { klasse: 'ok', text: t('recovery.info') },
};

export async function render() {
  const bericht = await api.recoveryReport();

  if (!bericht.items.length) {
    return el('div.empty', {}, [
      el('div.empty-mark', { style: 'background:var(--ok-soft);color:var(--ok)' }, [
        icon('checkCircle', { size: 26 }),
      ]),
      el('h2', { text: t('recovery.none_title') }),
      el('p.muted', {
        text: t('recovery.none_body'),
      }),
    ]);
  }

  return el('div.view-narrow', {}, [
    el('div.card', { style: 'margin-bottom:14px' }, [
      el('h2', { style: 'margin:0 0 6px;font-size:16px', text: t('recovery.title') }),
      el('p.small.muted', {
        style: 'margin:0',
        text: t('recovery.intro'),
      }),
      bericht.critical
        ? el('p.small', {
            style: 'margin:8px 0 0;color:var(--danger)',
            text: t('recovery.critical_count', { count: bericht.critical }),
          })
        : null,
    ]),

    ...bericht.items.map(karte),
  ]);
}

function karte(item) {
  const schwere = SCHWERE[item.severity] ?? SCHWERE.hinweis;

  const orte = el('div.list-preview', {}, [
    zeile(t('recovery.source'), item.src, item.src_exists),
    item.dst ? zeile(t('recovery.target'), item.dst, item.dst_exists) : null,
    item.temp ? zeile(t('recovery.temporary'), item.temp, item.temp_exists) : null,
  ]);

  return el('div.card', { style: 'margin-bottom:10px' }, [
    el('div.row', {}, [
      el('span', { class: `pill ${schwere.klasse}` }, [
        icon(item.severity === 'kritisch' ? 'alertTriangle' : 'info', { size: 12 }),
        el('span', { text: schwere.text }),
      ]),
      el('strong', { style: 'flex:1', text: item.original_name }),
      el('span.small.muted', { text: t('recovery.item_meta', { size: bytes(item.size), when: datumZeit(item.ts) }) }),
    ]),
    el('p', {
      style: 'margin:10px 0 0',
      text: t(`recovery.explanation.${item.diagnosis}`, { name: item.original_name }),
    }),
    orte,
    el(
      'div.row',
      { style: 'margin-top:10px' },
      item.choices.length
        ? item.choices.map((w) =>
            btn({
              icon: w.touches_data ? 'check' : 'x',
              text: t(`recovery.choice.${w.id}`),
              size: 'small',
              kind: w.touches_data ? 'primary' : '',
              title: w.touches_data
                ? t('recovery.choice_data')
                : t('recovery.choice_record'),
              onClick: () => waehlen(item, w),
            })
          )
        : [
            el('span.small.muted', {
              text: t('recovery.auto_close'),
            }),
          ]
    ),
  ]);
}

function zeile(label, pfad, existiert) {
  // Vorhanden oder nicht wird durch Symbol *und* Wort gezeigt, nicht nur
  // durch Farbe.
  return el('div.mit-icon', { style: 'gap:var(--s-2)' }, [
    el('span', { style: 'display:inline-block;width:14ch;flex:none', text: t('common.label_colon', { label }) }),
    el('span.grow.truncate', { title: pfad, text: kurzerPfad(pfad, 46) }),
    el('span.mit-icon', {
      style: `flex:none;color:var(--${existiert ? 'ok' : 'text-3'})`,
    }, [
      icon(existiert ? 'checkCircle' : 'xCircle', { size: 13 }),
      el('span', { text: existiert ? t('recovery.present') : t('recovery.missing') }),
    ]),
  ]);
}

async function waehlen(item, wahl) {
  const label = t(`recovery.choice.${wahl.id}`);
  if (wahl.touches_data) {
    const ok = await dialog({
      title: label,
      body: el('div', {}, [
        el('p', { text: t('recovery.affects', { name: item.original_name }) }),
        el('p.small.muted', {
          text: t('recovery.confirm_data'),
        }),
      ]),
      buttons: [
        { label: t('common.cancel'), value: false },
        { label, value: true, kind: 'primary' },
      ],
    });
    if (!ok) return;
  }

  try {
    await api.resolveRecovery(item.action_id, wahl.id);
    toast(t('recovery.resolved'), 'success');
    neuZeichnen();
  } catch (e) {
    toast(e?.title ?? t('common.action_failed'), 'error');
  }
}
