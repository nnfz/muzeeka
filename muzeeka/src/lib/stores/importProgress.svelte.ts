import { getContext, setContext } from 'svelte';

export interface ImportProgress {
  active: boolean;
  current: number;
  total: number;
  label: string;
}

const IMPORT_PROGRESS_KEY = Symbol('import-progress');

let _progress: ImportProgress = { active: false, current: 0, total: 0, label: '' };

export function getImportProgressStore() {
  return _progress;
}

export function setImportProgress(partial: Partial<ImportProgress>) {
  _progress = { ..._progress, ...partial };
}

export function resetImportProgress() {
  _progress = { active: false, current: 0, total: 0, label: '' };
}
