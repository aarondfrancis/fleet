import type { DiskStats } from "../bindings/DiskStats";
import type { HealthView } from "../bindings/HealthView";

export function memoryUsedFraction(health: HealthView | null | undefined): number | null {
  if (health?.memoryTotalBytes == null || health.memoryAvailableBytes == null) return null;
  return 1 - health.memoryAvailableBytes / Math.max(1, health.memoryTotalBytes);
}

/** Prefer the OS root, then the first reported filesystem. */
export function primaryDisk(health: HealthView | null | undefined): DiskStats | undefined {
  return health?.disks.find((disk) => disk.mountPoint === "/" || /^[A-Z]:[\\/]?$/i.test(disk.mountPoint))
    ?? health?.disks[0];
}

export function diskUsedFraction(disk: DiskStats | undefined): number | null {
  return disk ? 1 - disk.availableBytes / Math.max(1, disk.totalBytes) : null;
}
