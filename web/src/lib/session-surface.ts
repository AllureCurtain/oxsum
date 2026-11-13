// Which surface a session context resolves to (issue #1072).
//
// `SessionContextInfo` answers one shape for three situations: a live customer
// membership (`organizationId` set), a staff seat (`business` set — claimed or
// selectable), and a fully revoked identity (both null). The shell used to fold
// the third into "customer" and let `GET /session` 401 it back to `/login`,
// where a fresh login navigated `/` and bounced — a dead-end loop. Naming the
// arm here keeps the routing decision pure and lets the shell render a
// terminal "no access" state instead of the loop.

import type { SessionContextInfo } from "../api/hooks";

export type SessionSurface = "customer" | "business" | "revoked";

/**
 * The surface a session context lands on.
 *
 * - `business`: staff authority is the landing surface — either the session is
 *   already in `business` context, or it carries a staff seat with no customer
 *   membership left to select (`organizationId` null but `business` non-null).
 *   That second arm is why a bare `business != null` check is not enough: the
 *   schema also answers `context: "customer"` for a staff member whose
 *   memberships were all removed, and they still belong on the staff console.
 * - `revoked`: `organizationId` and `business` are both null — the identity
 *   holds neither a customer membership nor a staff seat. Nothing can serve
 *   it; the shell must not route it into either surface.
 * - `customer`: everything else — a real customer membership to land on.
 */
export function surfaceForContext(context: SessionContextInfo): SessionSurface {
  if (context.context === "business") return "business";
  if (context.organizationId === null) {
    return context.business == null ? "revoked" : "business";
  }
  return "customer";
}
