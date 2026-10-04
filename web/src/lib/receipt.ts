// The send receipt: what `POST /v1/messages` says about the recipients, in
// the words the CLI uses (`amail send`).

import type { Key } from './i18n';
import type { RecipientNote } from './types';

/** The text of a note: a dictionary key and its parameters (`until` is
 * filled in by the caller, formatted for the reader). */
export function noteKey(n: RecipientNote): { key: Key; params: Record<string, string> } {
  switch (n.state) {
    case 'dormant':
      return { key: n.wakes ? 'compose.note.dormantWakes' : 'compose.note.dormant', params: {} };
    case 'offline':
      return { key: 'compose.note.offline', params: {} };
    case 'focus':
      return { key: n.wakes ? 'compose.note.focusWakes' : 'compose.note.focus', params: { until: n.until ?? '' } };
    case 'wake_failed':
      return { key: 'compose.note.wakeFailed', params: {} };
    case 'turn_failed':
      return { key: 'compose.note.turnFailed', params: {} };
    default:
      return { key: 'compose.note.other', params: { state: n.state } };
  }
}
