import type { ReactNode } from "react";

// ---------------------------------------------------------------------------
// States — loading / empty / error placeholders (skeleton rows live in #414).
// ---------------------------------------------------------------------------

export function PageState(props: { children: ReactNode; icon?: ReactNode }) {
  return (
    <div className="flex flex-col items-center gap-2 px-4 py-10 text-center">
      {props.icon ? <div className="text-muted">{props.icon}</div> : null}
      <p className="text-sm text-muted">{props.children}</p>
    </div>
  );
}

export function EmptyRows(props: { colSpan: number; icon?: ReactNode; children: ReactNode }) {
  return (
    <tr>
      <td colSpan={props.colSpan} className="px-4 py-10 text-center text-sm text-muted">
        {props.icon ? <div className="mb-2 flex justify-center text-muted">{props.icon}</div> : null}
        {props.children}
      </td>
    </tr>
  );
}
