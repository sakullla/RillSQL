import { onBeforeUnmount, onMounted, ref } from "vue";
import { closeFromTitleBar, handleCloseRequested, releaseCloseWithoutHook } from "./close-guard";

/** True when this page is the Tauri webview, where the native title bar can be replaced. */
export function desktopShell(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

type AppWindow = {
  minimize: () => Promise<void>;
  toggleMaximize: () => Promise<void>;
  close: () => Promise<void>;
  isMaximized: () => Promise<boolean>;
  isFocused: () => Promise<boolean>;
  onResized: (handler: () => void) => Promise<() => void>;
  onFocusChanged: (handler: (event: { payload: boolean }) => void) => Promise<() => void>;
  onCloseRequested: (handler: (event: { preventDefault: () => void }) => void | Promise<void>) => Promise<() => void>;
};

async function currentWindow(): Promise<AppWindow> {
  const { getCurrentWindow } = await import("@tauri-apps/api/window");
  return getCurrentWindow();
}

/** Caption state for a borderless window. Browser previews keep the host window's own controls. */
export function useWindowChrome() {
  const customChrome = ref(desktopShell());
  const maximized = ref(false);
  const focused = ref(true);
  let stop: (() => void) | undefined;
  let cancelled = false;
  let closeHookInstalled = false;

  onMounted(() => {
    if (!customChrome.value) return;
    void (async () => {
      try {
        const app = await currentWindow();
        const [isMax, isFocused] = await Promise.all([app.isMaximized(), app.isFocused()]);
        if (cancelled) return;
        maximized.value = isMax;
        focused.value = isFocused;
        const stopResize = await app.onResized(() => {
          void app.isMaximized().then((value) => {
            if (!cancelled) maximized.value = value;
          });
        });
        const stopFocus = await app.onFocusChanged((event) => {
          if (!cancelled) focused.value = event.payload;
        });
        let stopClose: (() => void) | undefined;
        try {
          // While this listener exists, Tauri 2.12 prevent_close's synchronously.
          // Leaving preventDefault unset makes the API call destroy().
          stopClose = await app.onCloseRequested((event) => handleCloseRequested(event));
          closeHookInstalled = true;
        } catch {
          stopClose = undefined;
          closeHookInstalled = false;
        }
        if (cancelled) {
          stopResize();
          stopFocus();
          stopClose?.();
          closeHookInstalled = false;
          return;
        }
        stop = () => {
          stopResize();
          stopFocus();
          stopClose?.();
          closeHookInstalled = false;
        };
      } catch {
        if (!cancelled) customChrome.value = false;
      }
    })();
  });

  onBeforeUnmount(() => {
    cancelled = true;
    stop?.();
  });

  async function minimize() {
    if (!desktopShell()) return;
    try {
      await (await currentWindow()).minimize();
    } catch {
      /* leave the button in place when the window command is refused */
    }
  }

  async function toggleMaximize() {
    if (!desktopShell()) return;
    try {
      const app = await currentWindow();
      await app.toggleMaximize();
      maximized.value = await app.isMaximized();
    } catch {
      /* leave the button in place when the window command is refused */
    }
  }

  async function close() {
    await closeFromTitleBar(async () => {
      if (!desktopShell()) {
        releaseCloseWithoutHook();
        return;
      }
      try {
        await (await currentWindow()).close();
        if (!closeHookInstalled) releaseCloseWithoutHook();
      } catch {
        releaseCloseWithoutHook();
      }
    });
  }

  return { customChrome, maximized, focused, minimize, toggleMaximize, close };
}
