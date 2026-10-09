import { create } from "zustand";
import type { OpenGroups, Selection } from "./lib/nav";

/** UI-only state. Server data lives in TanStack Query. */
interface UiStore {
  selection: Selection;
  query: string;
  setQuery: (query: string) => void;
  collapsed: Set<string>;
  /** Collapsible sidebar groups. */
  open: OpenGroups;
  select: (s: Selection) => void;
  toggleCollapsed: (machineId: string, collapse?: boolean) => void;
  setOpen: (group: keyof OpenGroups, open: boolean) => void;
}

const COLLAPSED_KEY = "rd.collapsed";

function loadCollapsed(): Set<string> {
  try {
    return new Set(JSON.parse(localStorage.getItem(COLLAPSED_KEY) ?? "[]"));
  } catch {
    return new Set();
  }
}

export const useUi = create<UiStore>((set, get) => ({
  selection: { kind: "overview" },
  query: "",
  setQuery: (query) => set({ query }),
  collapsed: loadCollapsed(),
  open: { unlinked: false, other: false },
  select: (selection) => set({ selection }),
  toggleCollapsed: (id, collapse) => {
    const next = new Set(get().collapsed);
    const shouldCollapse = collapse ?? !next.has(id);
    if (shouldCollapse) next.add(id);
    else next.delete(id);
    try {
      localStorage.setItem(COLLAPSED_KEY, JSON.stringify([...next]));
    } catch {
      // Non-essential.
    }
    set({ collapsed: next });
  },
  setOpen: (group, open) => set((s) => ({ open: { ...s.open, [group]: open } })),
}));
