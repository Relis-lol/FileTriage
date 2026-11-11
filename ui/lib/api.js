// Bruecke zum Rust-Backend. Jede Funktion hier entspricht genau einem Befehl.

import { t } from './i18n.js';

const { invoke: tauriInvoke } = window.__TAURI__?.core ?? {};
const listenTo = window.__TAURI__?.event?.listen;

/**
 * Fehler aus dem Backend: {kind, message}. Titel und Hinweis kommen immer
 * aus dem aktiven Sprachkatalog.
 *
 * `title` und `hint` sind fuer den Nutzer, `message` ist die technische
 * Fassung und gehoert hinter "Details".
 */
export class BackendError extends Error {
  constructor(payload) {
    const objekt = typeof payload === 'object' && payload !== null;
    const message = objekt ? payload.message || t('common.unknown_error') : String(payload);
    super(message);
    this.name = 'BackendError';
    this.kind = objekt ? payload.kind || 'unknown' : 'unknown';
    const titleKey = `errors.${this.kind}.title`;
    const hintKey = `errors.${this.kind}.hint`;
    const translatedTitle = t(titleKey);
    const translatedHint = t(hintKey);
    this.title = translatedTitle === titleKey ? t('common.generic_failed') : translatedTitle;
    this.hint = translatedHint === hintKey ? '' : translatedHint;
  }
}

async function invoke(cmd, args) {
  if (!tauriInvoke) throw new BackendError(t('common.backend_missing'));
  try {
    return await tauriInvoke(cmd, args);
  } catch (e) {
    throw new BackendError(e);
  }
}

export const listen = (event, handler) =>
  listenTo ? listenTo(event, handler) : Promise.resolve(() => {});

export const appInfo = () => invoke('app_info');

// Lizenz. Die Oberflaeche zeigt den Zustand an; gesperrt wird im Kern.
export const licenseState = () => invoke('license_state');
export const openPurchase = () => invoke('open_purchase');

export const getConfig = () => invoke('get_config');
export const saveConfig = (config) => invoke('save_config', { config });

export const pickFolder = (title) => invoke('pick_folder', { title });

export const knownFolders = () => invoke('known_folders');
export const recentRoots = () => invoke('recent_roots');
export const forgetRecent = (path) => invoke('forget_recent', { path });
export const prepareRoot = (path) => invoke('prepare_root', { path });

export const listRoots = () => invoke('list_roots');
export const removeRoot = (id) => invoke('remove_root', { id });
export const startScan = (root) => invoke('start_scan', { root });
export const cancelScan = () => invoke('cancel_scan');
export const scanStatus = () => invoke('scan_status');

export const listFiles = (query) => invoke('list_files', { query });
export const categoryBreakdown = (query) => invoke('category_breakdown', { query });
export const overview = (root) => invoke('overview', { root });

export const moveToTarget = (path, targetKey, policy) =>
  invoke('move_to_target', { path, targetKey, policy });
export const moveToFolder = (path, folder, policy) =>
  invoke('move_to_folder', { path, folder, policy });
export const moveMany = (paths, folder, policy) => invoke('move_many', { paths, folder, policy });
export const deleteFile = (path, confirmPermanent) =>
  invoke('delete_file', { path, confirmPermanent });
export const deleteMany = (paths, confirmPermanent) =>
  invoke('delete_many', { paths, confirmPermanent });
export const renameFile = (path, newName) => invoke('rename_file', { path, newName });
export const setFileState = (path, newState) => invoke('set_file_state', { path, newState });

export const recoveryReport = () => invoke('recovery_report');
export const resolveRecovery = (actionId, choice) =>
  invoke('resolve_recovery', { actionId, choice });

export const listSessions = () => invoke('list_sessions');
export const currentSession = () => invoke('current_session');
export const planSessionUndo = (sessionId) => invoke('plan_session_undo', { sessionId });
export const undoSession = (sessionId) => invoke('undo_session', { sessionId });

export const undoLast = () => invoke('undo_last');
export const undoAction = (id) => invoke('undo_action', { id });
export const undoBatch = (batchId) => invoke('undo_batch', { batchId });
export const actionHistory = () => invoke('action_history');

export const preview = (path) => invoke('preview', { path });
export const openPath = (path) => invoke('open_path', { path });
export const revealPath = (path) => invoke('reveal_path', { path });

export const findDuplicates = (root, minSize) => invoke('find_duplicates', { root, minSize });
export const cancelDuplicates = () => invoke('cancel_duplicates');
export const duplicateStatus = () => invoke('duplicate_status');
export const duplicateGroups = (root, minSize) => invoke('duplicate_groups', { root, minSize });
export const acceptSuggestion = (suggestion) => invoke('accept_suggestion', { suggestion });

export const listRules = () => invoke('list_rules');
export const saveRule = (rule) => invoke('save_rule', { rule });
export const deleteRule = (id) => invoke('delete_rule', { id });
export const ruleSuggestions = () => invoke('rule_suggestions');

// Passive Vorschlaege aus wiederkehrenden Dateinamensmustern. Werden nur beim
// Oeffnen der Regelseite geholt - es gibt keinen Hintergrundlauf.
export const patternSuggestions = () => invoke('pattern_suggestions');
export const acceptPattern = (suggestion) => invoke('accept_pattern', { suggestion });
export const dismissPattern = (token, target) => invoke('dismiss_pattern', { token, target });
export const patternMatchCount = (suggestion, root) =>
  invoke('pattern_match_count', { suggestion, root });

// Aktuelle Trefferzahl je Regel. Zaehlt nur; veraendert nichts.
export const ruleMatchCounts = (root) => invoke('rule_match_counts', { root });

// Dateizahl und Gesamtgroesse mehrerer Ordner - ausschliesslich aus dem Index,
// ohne Scan.
export const folderSizes = (paths) => invoke('folder_sizes', { paths });
export const dismissSuggestion = (ext, category, target) =>
  invoke('dismiss_suggestion', { ext, category, target });
export const dryRun = (ruleId, root) => invoke('dry_run', { ruleId, root });
export const applyRule = (ruleId, root, policy, confirmedCount) =>
  invoke('apply_rule', { ruleId, root, policy, confirmedCount });
export const suggestTarget = (path) => invoke('suggest_target', { path });
