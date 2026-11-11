# Customer billing amounts on narrow screens

Issue #1019 reproduces the statement list compressing exact amounts into one
character per line at 390×844. The fix applies `whitespace-nowrap` only to this
list's header/body cells, preserving the existing TableShell scroll region.

Actual browser verification: document width 390, table visible width 325,
content width 565. The total/paid `1.000001 cr`, zero owed and ISO date each remain
on one line. A horizontal wheel scroll exposes the rightmost detail link;
clicking it loads the matching paid statement and its itemized lines. Desktop
1280×900 retains a 1005px table without document overflow. Screenshots inspected.
TypeScript/Vite build, pinned formatting and diff validation passed.

This isolated frontend branch deliberately excludes the independent overdue
notice repair #1018 / PR #1020. Its screenshots retain the original notice;
that text is not an assertion of debt. The backend is the #1017 development
fixture on the manifest's source, and the debug build serves this frontend
from disk. No production payment, genuine PSP or final-candidate claim.
