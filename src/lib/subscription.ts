/** Own an async event listener for one effect, including teardown during setup. */
export function subscribeToUpdates<T>(
  subscribe: (handler: (value: T) => void) => Promise<() => void>,
  onUpdate: (value: T) => void,
  onReady: () => void,
  onError: (error: unknown) => void = (error) => console.error("Couldn't listen for updates", error),
): () => void {
  let disposed = false;
  let unsubscribe: (() => void) | undefined;
  void subscribe((value) => {
    if (!disposed) onUpdate(value);
  }).then((off) => {
    if (disposed) off();
    else {
      unsubscribe = off;
      onReady();
    }
  }).catch((error) => {
    if (!disposed) onError(error);
  });
  return () => {
    disposed = true;
    unsubscribe?.();
    unsubscribe = undefined;
  };
}
