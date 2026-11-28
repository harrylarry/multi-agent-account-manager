import { Channel as TauriChannel, invoke as tauriInvoke } from "@tauri-apps/api/core";
import { listen as tauriListen, type EventCallback } from "@tauri-apps/api/event";

interface ManagerBridge {
  postMessage(message: { command: string; args: Record<string, unknown> }): Promise<unknown>;
}

const bridge = (window as Window & {
  webkit?: { messageHandlers?: { diehaoManager?: ManagerBridge } };
}).webkit?.messageHandlers?.diehaoManager;

export const isSwiftHost = bridge !== undefined;
let navigation: ((payload: unknown) => void) | undefined;

class SwiftNavigationChannel<T> {
  constructor(callback?: (payload: T) => void) { navigation = payload => callback?.(payload as T); }
  toJSON(): boolean { return true; }
}

export const Channel = bridge ? SwiftNavigationChannel : TauriChannel;
export type Channel<T> = SwiftNavigationChannel<T> | TauriChannel<T>;

if (bridge) {
  Object.defineProperty(window, "diehaoManagerNavigation", {
    value: (payload: unknown) => navigation?.(payload),
  });
}

export async function invoke<T>(command: string, args: Record<string, unknown> = {}): Promise<T> {
  if (!bridge) return tauriInvoke<T>(command, args);
  const payload: Record<string, unknown> = JSON.parse(JSON.stringify(args));
  return await bridge.postMessage({ command, args: payload }) as T;
}

export async function listen<T>(event: string, callback: EventCallback<T>): Promise<() => void> {
  // This slice exposes only request/reply and the navigation event channel.
  if (bridge) return () => {};
  return tauriListen(event, callback);
}
