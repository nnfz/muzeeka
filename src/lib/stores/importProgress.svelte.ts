export interface ImportProgress {
  active: boolean;
  current: number;
  total: number;
  label: string;
}

const initialProgress: ImportProgress = { active: false, current: 0, total: 0, label: '' };
let progress = $state<ImportProgress>({ ...initialProgress });

export function getImportProgressStore() {
  return progress;
}

export function setImportProgress(partial: Partial<ImportProgress>) {
  Object.assign(progress, partial);
}

export function resetImportProgress() {
  Object.assign(progress, initialProgress);
}
