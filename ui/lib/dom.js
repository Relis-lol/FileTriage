// Kleine DOM-Helfer. Bewusst kein Framework: die Oberflaeche ist eine Liste
// und eine Karte, dafuer lohnt keine Abhaengigkeit.

import { icon } from './icons.js';
import { t } from './i18n.js';

/**
 * Erzeugt ein Element.
 * @param {string} tag  z. B. "div.card" oder "button.btn.primary"
 * @param {object} [attrs]
 * @param {(Node|string)[]} [children]
 */
export function el(tag, attrs = {}, children = []) {
  const [name, ...classes] = tag.split('.');
  const node = document.createElement(name || 'div');
  if (classes.length) node.className = classes.join(' ');
  for (const [key, value] of Object.entries(attrs)) {
    if (value === null || value === undefined || value === false) continue;
    if (key === 'text') node.textContent = String(value);
    else if (key === 'html') node.innerHTML = value;
    else if (key.startsWith('on') && typeof value === 'function') {
      node.addEventListener(key.slice(2).toLowerCase(), value);
    } else if (key === 'dataset') {
      Object.assign(node.dataset, value);
    } else {
      node.setAttribute(key, value === true ? '' : String(value));
    }
  }
  for (const child of [].concat(children)) {
    if (child === null || child === undefined || child === false) continue;
    node.append(child instanceof Node ? child : document.createTextNode(String(child)));
  }
  return node;
}

export function clear(node) {
  while (node.firstChild) node.removeChild(node.firstChild);
  return node;
}

/**
 * Schaltfläche mit Symbol, Beschriftung und optional sichtbarem Tastenkürzel.
 *
 * Das Symbol unterstützt die Beschriftung, es ersetzt sie nicht: Ein Knopf, auf
 * dem nur ein Papierkorb abgebildet ist, zwingt zum Raten. Das Kürzel steht
 * direkt am Knopf — dann braucht niemand ein Tutorial, um es zu lernen.
 *
 * @param {{
 *   icon?: string, text?: string, kind?: string, size?: 'small'|'large',
 *   hotkey?: string, title?: string, ariaLabel?: string,
 *   onClick?: Function, disabled?: boolean, id?: string
 * }} o
 */
export function btn(o = {}) {
  const klassen = ['btn'];
  if (o.kind) klassen.push(o.kind);
  if (o.size) klassen.push(o.size);

  const knopf = el('button', {
    class: klassen.join(' '),
    type: 'button',
    onClick: o.onClick,
    disabled: o.disabled ? true : null,
    title: o.title ?? null,
    id: o.id ?? null,
    // Fehlt der Text, muss ein Name aus dem Label kommen - sonst hört ein
    // Screenreader nur "Schaltfläche".
    'aria-label': o.ariaLabel ?? (o.text ? null : o.title ?? null),
    'aria-keyshortcuts': o.hotkey ?? null,
  });

  if (o.icon) knopf.append(icon(o.icon, { size: o.size === 'large' ? 20 : 17 }));
  if (o.text) knopf.append(el('span', { text: o.text }));
  if (o.hotkey) knopf.append(el('kbd.hint', { text: o.hotkey, 'aria-hidden': 'true' }));
  return knopf;
}

/** Schaltfläche nur mit Symbol. Beschriftung ist Pflicht, nur unsichtbar. */
export function iconBtn(name, label, o = {}) {
  return btn({ ...o, icon: name, text: null, ariaLabel: label, title: o.title ?? label });
}

let toastTimer = null;

/**
 * Kurze Rueckmeldung am unteren Rand.
 * @param {string} message
 * @param {"info"|"success"|"error"} [kind]
 * @param {{label:string, onClick:Function}} [action]
 */
export function toast(message, kind = 'info', action = null) {
  const host = document.getElementById('toast');
  if (!host) return;
  clear(host);
  host.className = `toast ${kind} visible`;
  host.append(el('span', { text: message }));
  if (action) {
    host.append(
      el('button.toast-action', {
        text: action.label,
        onClick: () => {
          host.classList.remove('visible');
          action.onClick();
        },
      })
    );
  }
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => host.classList.remove('visible'), action ? 8000 : 4000);
}

/**
 * Modaler Dialog mit eigenen Schaltflaechen. Loest mit dem Wert der gewaehlten
 * Schaltflaeche auf, mit `null` bei Abbruch.
 * @param {{title:string, body?:Node|string, buttons:{label:string, value:any, kind?:string}[]}} opts
 */
export function dialog(opts) {
  return new Promise((resolve) => {
    const overlay = el('div.overlay');
    // Wohin der Fokus nach dem Schliessen zurueckkehrt.
    const vorher = document.activeElement;
    const schliessen = (value) => {
      overlay.remove();
      document.removeEventListener('keydown', beiTaste, true);
      if (vorher && typeof vorher.focus === 'function') vorher.focus();
      resolve(value);
    };
    const beiTaste = (event) => {
      if (event.key === 'Escape') {
        event.stopPropagation();
        schliessen(null);
        return;
      }
      // Fokus im Dialog halten: Wer mit der Tastatur bedient, soll nicht
      // versehentlich hinter dem Dialog landen und dort etwas auslösen.
      if (event.key === 'Tab') {
        const ziele = [...overlay.querySelectorAll('button, [href], input, select, textarea, summary')]
          .filter((n) => !n.disabled && n.offsetParent !== null);
        if (!ziele.length) return;
        const erster = ziele[0];
        const letzter = ziele[ziele.length - 1];
        if (event.shiftKey && document.activeElement === erster) {
          event.preventDefault();
          letzter.focus();
        } else if (!event.shiftKey && document.activeElement === letzter) {
          event.preventDefault();
          erster.focus();
        }
      }
    };
    document.addEventListener('keydown', beiTaste, true);

    const knoepfe = opts.buttons.filter(Boolean).map((b) =>
      el('button', {
        class: `btn ${b.kind || ''}`,
        type: 'button',
        text: b.label,
        onClick: () => schliessen(b.value),
      })
    );

    overlay.append(
      el('div.modal', { role: 'dialog', 'aria-modal': 'true' }, [
        el('h2', { text: opts.title }),
        opts.body ? el('div.modal-body', {}, [opts.body]) : null,
        el('div.modal-actions', {}, knoepfe),
      ])
    );
    overlay.addEventListener('mousedown', (e) => {
      if (e.target === overlay) schliessen(null);
    });
    document.body.append(overlay);
    knoepfe[0]?.focus();
  });
}

/**
 * Zeigt einen Backend-Fehler in normaler Sprache.
 *
 * Der Nutzer sieht die Überschrift und den Hinweis; die technische Fassung
 * steckt hinter „Details" und bleibt für den Fall da, dass jemand sie braucht.
 * Ein Fehler wird nur dann zum Dialog, wenn es etwas zu entscheiden gibt —
 * sonst genügt eine Meldung am unteren Rand.
 *
 * @param {{kind?:string,title?:string,hint?:string,message?:string}} fehler
 * @param {{label:string, value:any, kind?:string}[]} [aktionen]
 */
export function fehlerDialog(fehler, aktionen = null) {
  const titel = fehler?.title || t('common.generic_failed');
  const hinweis = fehler?.hint || '';
  const technisch = fehler?.message || String(fehler ?? '');

  if (!aktionen) {
    toast(hinweis ? `${titel} — ${hinweis}` : titel, 'error');
    return Promise.resolve(null);
  }

  const details = el('details.details', {}, [
    el('summary', { text: t('common.details') }),
    el('code.details-body', { text: technisch }),
  ]);

  return dialog({
    title: titel,
    body: el('div', {}, [hinweis ? el('p', { text: hinweis }) : null, details]),
    buttons: aktionen,
  });
}

/** Prueft, ob gerade in ein Eingabefeld getippt wird. */
export function tippt() {
  const a = document.activeElement;
  if (!a) return false;
  return (
    a.tagName === 'INPUT' ||
    a.tagName === 'TEXTAREA' ||
    a.tagName === 'SELECT' ||
    a.isContentEditable
  );
}
