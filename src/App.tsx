import { useEffect, useMemo } from "react";
import { DetailPane } from "./components/DetailPane";
import { Sidebar } from "./components/Sidebar";
import { ErrorText } from "./components/ui";
import { useFleet } from "./hooks";
import { filterNavView, flattenNav, moveSelection, reconcileSelection, selectionId } from "./lib/nav";
import { emitEvent } from "./lib/tauri";
import { Settings } from "./settings/Settings";
import { useUi } from "./store";
import { CopyFeedback } from "./components/CopyValue";

export function App() {
  const isSettings = window.location.hash === "#settings";
  useEffect(() => {
    // Show the native window once the first frame is painted.
    requestAnimationFrame(() => emitEvent("frontend-ready"));
  }, []);
  return <CopyFeedback>{isSettings ? <Settings /> : <Main />}</CopyFeedback>;
}

function Main() {
  const { data: view, error } = useFleet();
  const stored = useUi((s) => s.selection);
  const select = useUi((s) => s.select);
  const collapsed = useUi((s) => s.collapsed);
  const toggleCollapsed = useUi((s) => s.toggleCollapsed);
  const open = useUi((s) => s.open);
  const query = useUi((s) => s.query);

  const filtered = useMemo(() => (view ? filterNavView(view, query) : undefined), [view, query]);
  const searching = !!query.trim();
  const nav = useMemo(
    () => (filtered ? flattenNav(filtered, searching ? new Set() : collapsed, searching ? { unlinked: true, other: true } : open) : []),
    [filtered, searching, collapsed, open],
  );
  // Machines and runners come and go; a vanished selection shows the overview.
  const selection = view ? reconcileSelection(view, stored) : stored;

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const t = e.target;
      if (t instanceof HTMLInputElement || t instanceof HTMLTextAreaElement || e.metaKey || e.ctrlKey || e.altKey) return;
      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault();
        select(moveSelection(nav, selection, e.key === "ArrowDown" ? 1 : -1));
      } else if (e.key === "ArrowLeft") {
        e.preventDefault();
        if (selection.kind === "machine" && !collapsed.has(selection.id)) toggleCollapsed(selection.id, true);
        else if (selection.kind === "runner") {
          const parent = nav.find((n) => selectionId(n.selection) === selectionId(selection))?.parent;
          if (parent) select({ kind: "machine", id: parent });
        }
      } else if (e.key === "ArrowRight") {
        e.preventDefault();
        if (selection.kind === "machine" && collapsed.has(selection.id)) toggleCollapsed(selection.id, false);
      } else if (e.key === "Home") {
        select({ kind: "overview" });
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [nav, selection, select, collapsed, toggleCollapsed]);

  if (error) {
    return (
      <div className="p-8 text-[13px]">
        <p className="font-medium">Couldn't load the fleet.</p>
        <ErrorText error={error} className="mt-1" />
      </div>
    );
  }
  if (!view || !filtered) return <div className="h-full bg-bg" />;
  return (
    <div className="flex h-full">
      <Sidebar view={view} filtered={filtered} selection={selection} />
      <DetailPane view={view} selection={selection} />
    </div>
  );
}
