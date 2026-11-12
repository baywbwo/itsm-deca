import type { TicketAllowedAction, TicketListItem, TicketStatusCode } from '../api/types';

/**
 * What a drag-and-drop on the board means for one ticket.
 *
 * The board never decides a transition by itself: the server sends `allowedActions` per ticket
 * (permission, workflow rule, blocking work) and the drop only chooses among them. This helper
 * keeps that decision out of the component so it can be tested without a DOM.
 */
export type DropPlan =
  | { kind: 'ignore' }
  | { kind: 'refuse'; message: string }
  | { kind: 'dialog'; to: TicketStatusCode; action: TicketAllowedAction };

const sameColumn = (t: TicketListItem, to: TicketStatusCode) => t.status.code === to;

export function planDrop(ticket: TicketListItem, to: TicketStatusCode): DropPlan {
  // Dropping a card back on its own column is a no-op, not an error.
  if (sameColumn(ticket, to)) return { kind: 'ignore' };

  // Archived tickets are read-only history; the archive is never a transition target.
  if (ticket.status.archived) {
    return { kind: 'refuse', message: `${ticket.displayNumber} ada di arsip dan tidak dapat dipindah.` };
  }

  const action = ticket.allowedActions.find((a) => a.to.code === to);
  if (!action) {
    return { kind: 'refuse', message: `${ticket.displayNumber} tidak dapat dipindah ke status ini. Tidak ada perubahan.` };
  }
  // A blocked action is offered by the server with the reason (e.g. blocking related work);
  // show that reason instead of opening a dialog that can only fail.
  if (action.blockedBy) return { kind: 'refuse', message: action.blockedBy };

  // resolve/reopen need a mandatory note, so they always go through the dialog.
  return { kind: 'dialog', to, action };
}
