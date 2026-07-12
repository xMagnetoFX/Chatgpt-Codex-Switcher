import type { MouseEvent, ReactNode } from "react";
import {
  WindowCloseIcon,
  WindowMaximizeIcon,
  WindowMinimizeIcon,
  WindowRestoreIcon,
} from "./icons";

interface AppShellProps {
  isMacOs: boolean;
  isWindowMaximized: boolean;
  sidebar: ReactNode;
  topBar: ReactNode;
  children: ReactNode;
  onTitlebarDrag: (event: MouseEvent<HTMLDivElement>) => void;
  onTitlebarDoubleClick: () => void;
  onMinimize: () => void;
  onToggleMaximize: () => void;
  onClose: () => void;
}

export function AppShell({
  isMacOs,
  isWindowMaximized,
  sidebar,
  topBar,
  children,
  onTitlebarDrag,
  onTitlebarDoubleClick,
  onMinimize,
  onToggleMaximize,
  onClose,
}: AppShellProps) {
  return (
    <div
      className={`h-dvh overflow-hidden text-[color:var(--text-strong)] ${
        isWindowMaximized ? "app-shell-maximized" : ""
      }`}
    >
      <div className="app-backdrop flex h-full min-h-0 flex-col overflow-hidden">
        <div className="app-titlebar flex h-[42px] shrink-0 items-center px-3">
          <div
            onMouseDown={onTitlebarDrag}
            onDoubleClick={onTitlebarDoubleClick}
            data-testid="window-drag-region"
            className={`h-full flex-1 select-none cursor-default ${isMacOs ? "ml-18" : ""}`}
          />
          <span className="app-titlebar-label">ChatGPT Codex Switcher</span>
          {!isMacOs && (
            <div className="window-controls ml-3 flex h-full items-stretch">
              <button
                type="button"
                onClick={onMinimize}
                className="titlebar-button"
                title="Minimize"
                aria-label="Minimize"
              >
                <WindowMinimizeIcon className="h-8 w-[46px]" />
              </button>
              <button
                type="button"
                onClick={onToggleMaximize}
                className="titlebar-button"
                title={isWindowMaximized ? "Restore" : "Maximize"}
                aria-label={isWindowMaximized ? "Restore" : "Maximize"}
              >
                {isWindowMaximized ? (
                  <WindowRestoreIcon className="h-8 w-[46px]" />
                ) : (
                  <WindowMaximizeIcon className="h-8 w-[46px]" />
                )}
              </button>
              <button
                type="button"
                onClick={onClose}
                className="titlebar-button titlebar-button-close"
                title="Close"
                aria-label="Close"
              >
                <WindowCloseIcon className="h-8 w-[46px]" />
              </button>
            </div>
          )}
        </div>

        <div className="relative flex min-h-0 flex-1 flex-col overflow-hidden md:flex-row">
          {sidebar}
          <div className="app-content-pane flex min-h-0 min-w-0 flex-1 flex-col">
            {topBar}
            <main className="app-scrollbar app-main-scroll min-h-0 flex-1 overflow-y-auto px-4 py-4 md:px-5 md:py-5 xl:px-8 xl:py-8">
              <div className="app-main-frame">{children}</div>
            </main>
          </div>
        </div>
      </div>
    </div>
  );
}
