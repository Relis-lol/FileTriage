// Anwendungsgerüst: Navigation, geteilter Zustand, Tastatur, Drag and Drop.
//
// Grundsatz für den Aufbau: Beim ersten Start gibt es keinen Assistenten und
// keine Seitenleiste voller Ansichten, die noch nichts anzeigen können. Es gibt
// eine Frage — welcher Ordner? — und Antworten zum Anklicken.

import * as api from './lib/api.js';
import { clear, dialog, el, iconBtn, toast, tippt } from './lib/dom.js';
import { icon } from './lib/icons.js';
import { bytes, kurzerPfad } from './lib/format.js';

import * as home from './views/home.js';
import * as triage from './views/triage.js';
import * as duplicates from './views/duplicates.js';
import * as files from './views/files.js';
import * as rules from './views/rules.js';
import * as settings from './views/settings.js';
import * as recovery from './views/recovery.js';
import { t, ladeSprache, systemsprache } from './lib/i18n.js';

// Die Titel stehen als Schluessel da, nicht als Text: Sie werden bei jedem
// Zeichnen uebersetzt, damit ein Sprachwechsel sofort wirkt.
const VIEWS = {
  home: { modul: home, titel: 'nav.home', icon: 'home', gruppe: null },
  recovery: {
    modul: recovery,
    titel: 'nav.recovery',
    icon: 'shieldAlert',
    nurBeiBedarf: true,
    gruppe: null,
  },
  triage: { modul: triage, titel: 'nav.triage', icon: 'inbox', gruppe: null },
  duplicates: { modul: duplicates, titel: 'nav.duplicates', icon: 'copy', gruppe: 'nav.group.cleanup' },
  large: { modul: files, titel: 'nav.large', icon: 'drive', gruppe: 'nav.group.cleanup' },
  old: { modul: files, titel: 'nav.old', icon: 'clock', gruppe: 'nav.group.cleanup' },
  search: { modul: files, titel: 'nav.search', icon: 'search', gruppe: 'nav.group.cleanup' },
  rules: { modul: rules, titel: 'nav.rules', icon: 'rules', gruppe: 'nav.group.more' },
  settings: { modul: settings, titel: 'nav.settings', icon: 'settings', gruppe: 'nav.group.more' },
};

/** Geteilter Zustand. Views lesen ihn, ändern ihn nur über die Helfer hier. */
export const store = {
  config: null,
  info: null,
  root: null,
  rootName: null,
  roots: [],
  view: 'home',
  scan: { running: false, progress: null },
  recoveryCount: 0,
  offen: 0,
  /** Wird von der aktiven Ansicht gesetzt, um Tasten zu behandeln. */
  keyHandler: null,
};

let viewHost = null;
let sidebarHost = null;
let topbarHost = null;
let appHost = null;

export async function ladeConfig() {
  store.config = await api.getConfig();
  wendeThemaAn();
  return store.config;
}

export async function speichereConfig(config) {
  store.config = await api.saveConfig(config);
  wendeThemaAn();
  return store.config;
}

function wendeThemaAn() {
  const thema = store.config?.theme ?? 'system';
  const dunkel =
    thema === 'dark' ||
    (thema === 'system' && window.matchMedia('(prefers-color-scheme: dark)').matches);
  document.documentElement.dataset.theme = dunkel ? 'dark' : 'light';
}

export async function ladeRoots() {
  store.roots = await api.listRoots();
  return store.roots;
}

/** Gibt es überhaupt schon etwas zu zeigen, oder ist dies der erste Start? */
function ersterStart() {
  return !store.root && !(store.config?.recent_roots?.length > 0);
}

/** Wechselt die Ansicht. `params` geht an das Ansichtsmodul durch. */
export async function gehZu(view, params = {}) {
  if (!VIEWS[view]) view = 'home';
  store.view = view;
  store.keyHandler = null;

  // Beim ersten Start bleibt die Seitenleiste weg: Ansichten, die ohne Ordner
  // nichts anzeigen können, sind kein Angebot, sondern Ballast.
  const solo = ersterStart() && view === 'home';
  appHost.classList.toggle('solo', solo);
  sidebarHost.style.display = solo ? 'none' : '';
  topbarHost.style.display = solo ? 'none' : '';

  if (!solo) {
    zeichneSidebar();
    zeichneTopbar();
  }
  viewHost.className = 'view';
  clear(viewHost);
  try {
    const knoten = await VIEWS[view].modul.render({ view, params, host: viewHost });
    if (knoten) viewHost.append(knoten);
  } catch (e) {
    viewHost.append(
      el('div.empty', {}, [
        el('div.empty-mark', {}, [icon('alertTriangle', { size: 26 })]),
        el('h2', { text: t('common.view_failed') }),
        el('p', { text: e?.title || t('common.generic_failed') }),
      ])
    );
  }
}

/** Zeichnet die aktuelle Ansicht neu (nach Aktionen). */
export const neuZeichnen = (params = {}) => gehZu(store.view, params);

function zeichneSidebar() {
  clear(sidebarHost);
  sidebarHost.append(
    el('div.brand', {}, [
      el('span.mark', {}, [icon('inbox', { size: 15, strich: 2 })]),
      el('span', { text: t('app.name') }),
    ])
  );

  let letzteGruppe = null;
  for (const [key, def] of Object.entries(VIEWS)) {
    if (def.nurBeiBedarf && !store.recoveryCount && store.view !== key) continue;

    if (def.gruppe && def.gruppe !== letzteGruppe) {
      sidebarHost.append(el('div.nav-label', { text: t(def.gruppe) }));
      letzteGruppe = def.gruppe;
    }

    const aktiv = store.view === key;
    const zaehler =
      key === 'triage' && store.offen ? String(store.offen) :
      key === 'recovery' && store.recoveryCount ? String(store.recoveryCount) : null;

    sidebarHost.append(
      el(
        'button',
        {
          class: `nav-item${aktiv ? ' active' : ''}${
            key === 'recovery' && store.recoveryCount ? ' attention' : ''
          }`,
          type: 'button',
          'aria-current': aktiv ? 'page' : null,
          onClick: () => gehZu(key),
        },
        [
          icon(def.icon, { size: 17 }),
          el('span', { text: t(def.titel) }),
          zaehler ? el('span.count', { text: zaehler }) : null,
        ]
      )
    );
  }

  sidebarHost.append(el('div.nav-spacer'));
  sidebarHost.append(
    el('div.sidebar-foot', {}, [
      el('div', { text: t('app.version', { version: store.info?.version ?? '' }) }),
      el('div', { text: t('app.local_only') }),
    ])
  );
}

/**
 * Der Lizenzhinweis in der Kopfzeile.
 *
 * Bewusst klein, bewusst passiv, bewusst nur waehrend der Testphase. Kein
 * Fenster beim Start, kein Countdown, keine taegliche Erinnerung. Wer bezahlt
 * hat, sieht hier gar nichts - das ist der Normalzustand und braucht keine
 * Bestaetigung.
 */
function lizenzHinweis() {
  if (store.lizenz?.channel === 'community') return null;
  const z = store.lizenz?.state;
  if (!z || z.kind !== 'trial_active') return null;

  const tage = z.days_left;
  const text =
    tage === null || tage === undefined
      ? t('license.trial')
      : tage <= 0
        ? t('license.trial_last_day')
        : t('license.trial_left', { count: tage });

  return el('span.lizenz-chip', {}, [
    el('span', { text }),
    el('button.btn.ghost.small', {
      type: 'button',
      text: t('license.buy', { price: store.lizenz.price_hint }),
      onClick: kaufen,
    }),
  ]);
}

/**
 * Oeffnet die Produktseite im Microsoft Store.
 *
 * Solange die Store-Produktkennung fehlt, meldet der Kern einen Fehler. Dann
 * wird erklaert, wie man das Programm findet - statt einen erfundenen Link zu
 * oeffnen, der ins Leere fuehrt.
 */
export async function kaufen() {
  try {
    await api.openPurchase();
  } catch {
    await dialog({
      title: t('license.buy_failed'),
      body: el('p', { text: t('license.buy_failed_hint') }),
      buttons: [{ label: t('common.close'), value: null }],
    });
  }
}

function zeichneTopbar() {
  clear(topbarHost);
  topbarHost.append(el('h1', { text: t(VIEWS[store.view].titel) }));

  if (store.root) {
    topbarHost.append(
      el('span.root-chip', { title: store.root }, [
        icon('folder', { size: 14 }),
        el('span.truncate', { text: store.rootName || kurzerPfad(store.root, 40) }),
      ])
    );
  }
  topbarHost.append(el('div.grow'));
  const hinweis = lizenzHinweis();
  if (hinweis) topbarHost.append(hinweis);

  if (store.scan.running) {
    const p = store.scan.progress;
    topbarHost.append(
      el('span.small.muted', {
        // Die Groesse wird beim Scan ohnehin mitgezaehlt - sie zu zeigen
        // kostet nichts und sagt mehr als die blosse Anzahl.
        text: p?.files
          ? t('scan.progress', { count: p.files, size: bytes(p.bytes ?? 0) })
          : t('scan.searching'),
      }),
      el('button.btn.small', { type: 'button', text: t('common.cancel'), onClick: () => api.cancelScan() })
    );
  } else {
    topbarHost.append(
      iconBtn('folderOpen', t('home.other_folder'), {
        kind: 'ghost',
        size: 'small',
        onClick: ordnerWaehlen,
      }),
      store.root
        ? iconBtn('refresh', t('scan.rescan'), {
            kind: 'ghost',
            size: 'small',
            onClick: () => scanStarten(store.root),
          })
        : null
    );
  }
}

/** Öffnet den Windows-Ordnerdialog und übernimmt die Auswahl. */
export async function ordnerWaehlen() {
  const ordner = await api.pickFolder(t('app.pick_cleanup_folder'));
  if (!ordner) return null;
  return ordnerUebernehmen(ordner);
}

/**
 * Übernimmt einen Ordner — egal ob aus dem Dialog, aus der Liste oder
 * hineingezogen. Das Backend prüft ihn und richtet beim ersten Mal Ziele ein.
 * Es wird dabei keine Datei angefasst.
 */
export async function ordnerUebernehmen(pfad, { starten = true } = {}) {
  try {
    const vorbereitet = await api.prepareRoot(pfad);
    store.root = vorbereitet.path;
    store.rootName = vorbereitet.name;
    await ladeConfig();
    if (starten) await scanStarten(vorbereitet.path);
    await gehZu('triage');
    return vorbereitet;
  } catch (e) {
    toast(e?.title ? `${e.title} — ${e.hint || ''}`.trim() : t('common.generic_failed'), 'error');
    return null;
  }
}

export async function scanStarten(root) {
  try {
    await api.startScan(root);
    store.scan.running = true;
    store.root = root;
    if (store.view !== 'home') zeichneTopbar();
  } catch (e) {
    toast(e?.title ?? t('app.scan_failed'), 'error');
  }
}

/** Undo mit Rückmeldung — überall verfügbar. */
export async function undoLetzte() {
  try {
    const r = await api.undoLast();
    if (!r) {
      toast(t('app.undo_none'));
      return null;
    }
    if (r.ok) toast(t('app.undo_restored', { name: r.restored_to.split('\\').pop() }), 'success');
    else toast(t('app.undo_failed'), 'error');
    return r;
  } catch (e) {
    toast(e?.title ?? t('app.undo_failed'), 'error');
    return null;
  }
}

function globaleTaste(event) {
  if (tippt()) return;
  const cfg = store.config;

  const undoTaste = cfg?.hotkeys?.undo ?? 'Backspace';
  if ((event.ctrlKey && event.key.toLowerCase() === 'z') || event.key === undoTaste) {
    event.preventDefault();
    undoLetzte().then(() => neuZeichnen());
    return;
  }
  if (event.ctrlKey && event.key.toLowerCase() === 'f') {
    event.preventDefault();
    gehZu('search');
    return;
  }
  if (event.ctrlKey && event.key.toLowerCase() === 'o') {
    event.preventDefault();
    ordnerWaehlen();
    return;
  }
  if (store.keyHandler) store.keyHandler(event);
}

// ---------------------------------------------------------------------------
// Drag and Drop
// ---------------------------------------------------------------------------

/**
 * Ein hineingezogener Ordner wird angenommen — aber nichts wird dadurch
 * verschoben, kopiert oder gelöscht. Der Drop wählt nur den Arbeitsordner.
 */
async function richteDropEin() {
  const zone = el('div.dropzone', { id: 'dropzone' }, [
    el('div.ziel', {}, [
      icon('folderOpen', { size: 40, strich: 1.5 }),
      el('div', { style: 'font-size:var(--t-md);font-weight:600', text: t('drop.title') }),
      el('div.small.muted', { text: t('drop.hint') }),
    ]),
  ]);
  document.body.append(zone);
  const zeigen = (an) => zone.classList.toggle('aktiv', an);

  // Tauri liefert echte Dateipfade nur über sein eigenes Ereignis; das
  // Browser-Drop-Ereignis kennt aus Sicherheitsgründen keine Pfade.
  const tauri = window.__TAURI__?.event;
  if (tauri?.listen) {
    await tauri.listen('tauri://drag-enter', () => zeigen(true));
    await tauri.listen('tauri://drag-leave', () => zeigen(false));
    await tauri.listen('tauri://drag-drop', async (e) => {
      zeigen(false);
      const pfade = e?.payload?.paths ?? [];
      if (!pfade.length) return;
      if (pfade.length > 1) {
        toast(t('app.drop_single'));
        return;
      }
      await ordnerUebernehmen(pfade[0]);
    });
  }

  // Der Browser-Teil verhindert, dass ein Fehlwurf die Seite ersetzt.
  window.addEventListener('dragover', (e) => e.preventDefault());
  window.addEventListener('drop', (e) => e.preventDefault());
}

/**
 * Wechselt die Sprache und zeichnet neu.
 *
 * Ohne Neustart: Die Oberflaeche wird ohnehin bei jedem Wechsel neu
 * aufgebaut, also gibt es nichts, was eine alte Sprache festhalten koennte.
 * `null` bedeutet "Sprache des Systems".
 */
export async function setzeSprache(kennung) {
  const cfg = { ...store.config, language: kennung };
  store.config = await api.saveConfig(cfg);
  await ladeSprache(kennung || systemsprache());
  await neuZeichnen();
  zeichneSidebar();
  zeichneTopbar();
}

// ---------------------------------------------------------------------------
// Start
// ---------------------------------------------------------------------------

async function start() {
  appHost = document.getElementById('app');
  clear(appHost);
  sidebarHost = el('nav.sidebar', { 'aria-label': t('nav.aria') });
  topbarHost = el('header.topbar');
  viewHost = el('main.view', { id: 'view' });
  appHost.append(sidebarHost, el('div.main', {}, [topbarHost, viewHost]));
  document.body.append(el('div', { id: 'toast', class: 'toast', role: 'status' }));

  try {
    store.info = await api.appInfo();
    await ladeConfig();
    // Die Sprache steht vor allem anderen fest: Danach ist jeder Text, der
    // gezeichnet wird, schon in der richtigen Sprache. Ein Nachladen mitten
    // im Aufbau haette eine halb uebersetzte Oberflaeche ergeben.
    await ladeSprache(store.config.language || systemsprache());
    store.lizenz = await api.licenseState().catch(() => null);
    await ladeRoots();
  } catch (e) {
    appHost.append(el('div.empty', { text: t('common.start_failed', { error: t('common.generic_failed') }) }));
    return;
  }

  // Meldung vom Start - etwa ein neu angelegter Index. Kommt genau einmal
  // und nur, wenn wirklich etwas passiert ist.
  if (store.info?.notice) {
    const key = `startup.${store.info.notice}`;
    const message = t(key);
    toast(message === key ? t('startup.unknown_notice') : message, 'info');
  }

  store.root = store.config.last_root || null;
  store.rootName = store.root ? store.root.split('\\').filter(Boolean).pop() : null;

  window.matchMedia('(prefers-color-scheme: dark)').addEventListener('change', wendeThemaAn);
  document.addEventListener('keydown', globaleTaste);
  await richteDropEin();

  await api.listen('scan:progress', ({ payload }) => {
    store.scan.progress = payload;
    store.scan.running = !payload.done;
    if (store.view !== 'home' || !ersterStart()) zeichneTopbar();
    if (payload.done) {
      ladeRoots().then(() => {
        if (['home', 'triage', 'large', 'old', 'search'].includes(store.view)) neuZeichnen();
      });
    }
  });

  await api.listen('index:changed', () => {
    if (['home', 'large', 'old', 'search'].includes(store.view)) neuZeichnen();
  });

  await api.listen('index:rescan', ({ payload }) => {
    const key = payload ? `scan.rescan_reason.${payload}` : 'scan.rescan_notice';
    const message = t(key);
    toast(message === key ? t('scan.rescan_notice') : message);
  });

  const status = await api.scanStatus();
  store.scan.running = status.running;
  store.scan.progress = status.progress;

  // Unterbrochene Vorgänge haben Vorrang: Solange unklar ist, wo eine Datei
  // liegt, soll niemand weiter sortieren.
  const offen = await pruefeWiederherstellung();
  if (offen && offen.items.length) {
    await gehZu('recovery');
    return;
  }

  await gehZu('home');
}

/** Fragt nach unterbrochenen Vorgängen und merkt sich die Anzahl. */
export async function pruefeWiederherstellung() {
  try {
    const bericht = await api.recoveryReport();
    store.recoveryCount = bericht.items.length;
    return bericht;
  } catch {
    store.recoveryCount = 0;
    return null;
  }
}

start();
