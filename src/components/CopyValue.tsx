import { createContext, useContext, useEffect, useRef, useState, type ReactNode } from "react";
import { copyText } from "../lib/clipboard";

const Feedback = createContext<(message: string, failed?: boolean) => void>(() => {});

/** One small, accessible confirmation for the latest copy in this window. */
export function CopyFeedback({ children }: { children: ReactNode }) {
  const [notice, setNotice] = useState<{ message: string; failed: boolean } | null>(null);
  useEffect(() => {
    if (!notice) return;
    const timer = window.setTimeout(() => setNotice(null), notice.failed ? 4000 : 2000);
    return () => window.clearTimeout(timer);
  }, [notice]);
  return <Feedback.Provider value={(message, failed = false) => setNotice({ message, failed })}>
    {children}
    <div role="status" aria-live="polite" aria-atomic="true" className="pointer-events-none fixed inset-x-4 bottom-5 z-50 flex justify-center">
      {notice && <div className={`flex max-w-full items-center gap-2 rounded-lg border border-line bg-card px-3 py-2 text-[12.5px] shadow-lg ${notice.failed ? "text-bad" : "text-ink"}`}>
        <span aria-hidden className={notice.failed ? "text-bad" : "text-ok"}>{notice.failed ? "!" : "✓"}</span>
        <span>{notice.message}</span>
      </div>}
    </div>
  </Feedback.Provider>;
}

/** Copy the full underlying value, including when its visible text is truncated. */
export function CopyValue({ value, label, children, className = "", truncate = false, showIcon = true }: {
  value: string;
  label: string;
  children?: ReactNode;
  className?: string;
  truncate?: boolean;
  showIcon?: boolean;
}) {
  const feedback = useContext(Feedback);
  const pending = useRef(false);
  return <button
    type="button"
    className={`copy-value group/copy inline-flex max-w-full min-w-0 items-center gap-1.5 rounded align-baseline text-left hover:text-ink ${className}`}
    title={`${value}\nClick to copy`}
    aria-label={`Copy ${label.toLowerCase()}`}
    onClick={async (event) => {
      event.stopPropagation();
      const selection = window.getSelection();
      // Dragging to select a portion remains useful, especially in errors and paths.
      if (selection && !selection.isCollapsed && selection.anchorNode && event.currentTarget.contains(selection.anchorNode)) return;
      if (pending.current) return;
      pending.current = true;
      try {
        await copyText(value);
        feedback(`${label} copied`);
      } catch {
        feedback("Couldn't copy. Select the text and copy it manually.", true);
      } finally {
        pending.current = false;
      }
    }}
  >
    <span className={`min-w-0 ${truncate ? "truncate" : "break-words"} group-hover/copy:underline group-hover/copy:decoration-dotted group-hover/copy:underline-offset-2`}>{children ?? value}</span>
    {showIcon && <svg aria-hidden="true" className="shrink-0 text-ink-3 opacity-0 transition-opacity group-hover/copy:opacity-100 group-focus-visible/copy:opacity-100" width="12" height="12" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" strokeLinejoin="round">
      <rect x="5" y="5" width="8" height="9" rx="1.5" /><path d="M10 5V3.5A1.5 1.5 0 0 0 8.5 2h-5A1.5 1.5 0 0 0 2 3.5v6A1.5 1.5 0 0 0 3.5 11H5" />
    </svg>}
  </button>;
}
