import { useId, useState, type ReactNode } from "react";

/** A tab bar over full-height panels, keyed and ordered by tab name. Arrow keys, Home and End move between tabs. */
export function Tabs<T extends string>({ label, panels }: { label: string; panels: Record<T, ReactNode> }) {
  const names = Object.keys(panels) as T[];
  const [current, setCurrent] = useState<T>(names[0]);
  const id = useId();
  const tabId = (name: T) => `${id}-tab-${name}`;
  const panelId = (name: T) => `${id}-panel-${name}`;
  return (
    <div className="flex h-full min-w-0 flex-col bg-bg">
      <nav role="tablist" aria-label={label} className="flex shrink-0 gap-1 border-b border-line px-5 py-2">
        {names.map((name, index) => (
          <button
            key={name}
            id={tabId(name)}
            role="tab"
            aria-selected={current === name}
            aria-controls={panelId(name)}
            tabIndex={current === name ? 0 : -1}
            className={`rounded-[6px] px-3 py-1.5 text-[12.5px] font-medium focus-visible:ring-2 focus-visible:ring-accent ${current === name ? "bg-selected text-ink" : "text-ink-2 hover:bg-hover"}`}
            onClick={() => setCurrent(name)}
            onKeyDown={(event) => {
              const next = event.key === "ArrowRight" ? (index + 1) % names.length
                : event.key === "ArrowLeft" ? (index + names.length - 1) % names.length
                : event.key === "Home" ? 0 : event.key === "End" ? names.length - 1 : null;
              if (next === null) return;
              event.preventDefault();
              setCurrent(names[next]);
              document.getElementById(tabId(names[next]))?.focus();
            }}
          >
            {name}
          </button>
        ))}
      </nav>
      <div className="min-h-0 min-w-0 flex-1">
        {names.map((name) => (
          <section key={name} id={panelId(name)} role="tabpanel" aria-labelledby={tabId(name)} hidden={current !== name} className="scroll h-full px-5 py-4">
            {panels[name]}
          </section>
        ))}
      </div>
    </div>
  );
}
