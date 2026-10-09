import type { ReactNode } from "react";
import { Icon } from "./icons";
import { Card } from "./ui";

export interface Column {
  label: string;
  /** Padding and width; the first column uses `px-4`, the rest `px-3`. */
  className?: string;
}

/** A table of rows that each open a page, with a column left for the hover chevron. */
export function LinkTable({ columns, minWidth, children }: { columns: Column[]; minWidth: string; children: ReactNode }) {
  return (
    <Card className="overflow-x-auto">
      <table className={`w-full text-[13px] ${minWidth}`}>
        <thead className="border-b border-line bg-sunken/40 text-left text-[12px] text-ink-2">
          <tr>
            {columns.map((c, i) => <th key={c.label} scope="col" className={`py-2 font-medium ${c.className ?? (i === 0 ? "px-4" : "px-3")}`}>{c.label}</th>)}
            <th scope="col" className="w-8"><span className="sr-only">Open</span></th>
          </tr>
        </thead>
        <tbody className="divide-y divide-line">{children}</tbody>
      </table>
    </Card>
  );
}

/** A row that opens its page from anywhere on it; `opener` is its keyboard-reachable button. */
export function LinkRow({ onOpen, opener, title, children }: { onOpen: () => void; opener: ReactNode; title?: string; children: ReactNode }) {
  return (
    <tr className="group hover:bg-hover" onClick={onOpen}>
      <td className="px-4 py-2.5">
        <button className="flex max-w-[220px] items-center gap-2.5 text-left" title={title} onClick={(e) => { e.stopPropagation(); onOpen(); }}>
          {opener}
        </button>
      </td>
      {children}
      <td className="px-2 text-ink-3"><span className="opacity-0 group-hover:opacity-100"><Icon name="chevron" /></span></td>
    </tr>
  );
}
