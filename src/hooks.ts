import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import type { FleetView } from "./bindings/FleetView";
import { subscribeToUpdates } from "./lib/subscription";
import { commands, onConfigUpdated, onFleetUpdated } from "./lib/tauri";

/** The fleet view. Rust pushes updates; we never poll from here. */
export function useFleet() {
  const qc = useQueryClient();
  useEffect(() => {
    return subscribeToUpdates(
      onFleetUpdated,
      (view) => qc.setQueryData<FleetView>(["fleet"], view),
      // Updates pushed before the listener attached would be missed; fetch once more.
      () => { void qc.invalidateQueries({ queryKey: ["fleet"] }); },
    );
  }, [qc]);
  return useQuery({ queryKey: ["fleet"], queryFn: commands.getFleet, staleTime: Infinity });
}

export function useConfig() {
  const qc = useQueryClient();
  useEffect(() => {
    const refresh = () => { void qc.invalidateQueries({ queryKey: ["config"] }); };
    return subscribeToUpdates<void>(onConfigUpdated, refresh, refresh);
  }, [qc]);
  return useQuery({ queryKey: ["config"], queryFn: commands.getConfig });
}

/** Shared ticking clock so timers advance together. */
const listeners = new Set<(n: number) => void>();
let timer: ReturnType<typeof setInterval> | null = null;

export function useNow(): number {
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    listeners.add(setNow);
    if (!timer) timer = setInterval(() => listeners.forEach((l) => l(Date.now())), 1000);
    return () => {
      listeners.delete(setNow);
      if (listeners.size === 0 && timer) {
        clearInterval(timer);
        timer = null;
      }
    };
  }, []);
  return now;
}
