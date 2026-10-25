// Calls of the task API that the pages share, and how their errors are shown.

import { ApiError, del, get, patch, post, qs } from './api';
import { taskErrorCode } from './tasks';
import { t } from './ui.svelte';
import type { TaskDetail, TaskList, TaskPriority } from './types';

export const taskPath = (num: number, rest = '') => `/v1/tasks/${num}${rest}`;

export function getTask(num: number): Promise<TaskDetail> {
  return get<TaskDetail>(taskPath(num));
}

export function patchTask(num: number, body: Record<string, unknown>): Promise<TaskDetail> {
  return patch<TaskDetail>(taskPath(num), body);
}

export function setPriority(num: number, priority: TaskPriority): Promise<TaskDetail> {
  return patchTask(num, { priority });
}

export function setRank(num: number, rank: number): Promise<TaskDetail> {
  return patchTask(num, { rank });
}

export function addDep(num: number, blockedBy: number): Promise<TaskDetail> {
  return post<TaskDetail>(taskPath(num, '/deps'), { blocked_by: blockedBy });
}

export function removeDep(num: number, other: number): Promise<TaskDetail> {
  return del<TaskDetail>(taskPath(num, `/deps/${other}`));
}

export function listTasks(params: Record<string, string | number | undefined>): Promise<TaskList> {
  return get<TaskList>(`/v1/tasks${qs(params)}`);
}

/** An error of a task call in words: the known codes have their own text,
 * others show the server's message and code. */
export function taskError(e: unknown): string {
  if (e instanceof ApiError) {
    const code = taskErrorCode(e.status, e.code);
    if (code) return `${t(`task.err.${code}`)} (${e.message})`;
    return `${e.message} (${e.code})`;
  }
  return e instanceof Error ? e.message : String(e);
}
