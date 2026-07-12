import { useState, useEffect, useCallback } from "react";
import type { Update } from "@tauri-apps/plugin-updater";
import { isTauriRuntime } from "../lib/platform";
import { ArrowUpIcon, CheckIcon, CloseIcon } from "./icons";

type UpdateStatus =
  | { kind: "idle" }
  | { kind: "checking" }
  | { kind: "available"; update: Update }
  | { kind: "downloading"; downloaded: number; total: number | null }
  | { kind: "ready" }
  | { kind: "error"; message: string };

export function UpdateChecker() {
  const [status, setStatus] = useState<UpdateStatus>({ kind: "idle" });
  const [dismissed, setDismissed] = useState(false);

  const checkForUpdate = useCallback(async () => {
    if (!isTauriRuntime()) return;

    try {
      setStatus({ kind: "checking" });
      setDismissed(false);
      const { check } = await import("@tauri-apps/plugin-updater");
      const update = await check();
      if (update) {
        setStatus({ kind: "available", update });
      } else {
        setStatus({ kind: "idle" });
      }
    } catch (err) {
      console.error("Update check failed:", err);
      setStatus({ kind: "idle" });
    }
  }, []);

  useEffect(() => {
    if (!isTauriRuntime()) return;
    void checkForUpdate();
  }, [checkForUpdate]);

  const handleDownloadAndInstall = async () => {
    if (status.kind !== "available") return;
    const { update } = status;

    try {
      if (!isTauriRuntime()) return;
      let downloaded = 0;
      let total: number | null = null;

      await update.downloadAndInstall((event) => {
        switch (event.event) {
          case "Started":
            total = event.data.contentLength ?? null;
            setStatus({ kind: "downloading", downloaded: 0, total });
            break;
          case "Progress":
            downloaded += event.data.chunkLength;
            setStatus({ kind: "downloading", downloaded, total });
            break;
          case "Finished":
            setStatus({ kind: "ready" });
            break;
        }
      });

      setStatus({ kind: "ready" });
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      console.error("Update install failed:", err);
      setStatus({ kind: "error", message });
    }
  };

  const handleRelaunch = async () => {
    try {
      if (!isTauriRuntime()) return;
      const { relaunch } = await import("@tauri-apps/plugin-process");
      await relaunch();
    } catch (err) {
      console.error("Relaunch failed:", err);
    }
  };

  if (!isTauriRuntime()) {
    return null;
  }

  if (status.kind === "idle" || status.kind === "checking" || dismissed) {
    return null;
  }

  const formatBytes = (bytes: number) => {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  };

  return (
    <div className="fixed bottom-6 right-6 z-50 max-w-md">
      <div className="panel-surface rounded-[24px] p-4">
        {status.kind === "available" && (
          <div className="flex gap-3">
            <div className="mt-1 flex h-10 w-10 shrink-0 items-center justify-center rounded-2xl border border-[color:var(--accent-border)] bg-[color:var(--accent-soft)] text-[color:var(--accent)]">
              <ArrowUpIcon className="h-5 w-5" />
            </div>
            <div className="min-w-0 flex-1">
              <div className="section-kicker">Update ready</div>
              <p className="mt-2 text-sm font-semibold text-[color:var(--text-strong)]">
                Version {status.update.version} is available
              </p>
              {status.update.body && (
                <p className="mt-1 text-xs leading-6 text-[color:var(--text-muted)]">
                  {status.update.body}
                </p>
              )}
              <div className="mt-4 flex gap-2">
                <button
                  onClick={() => setDismissed(true)}
                  className="toolbar-button !min-h-10 justify-center px-3 text-xs"
                >
                  Later
                </button>
                <button
                  onClick={() => {
                    void handleDownloadAndInstall();
                  }}
                  className="toolbar-button toolbar-button-primary !min-h-10 justify-center px-3 text-xs"
                >
                  Update now
                </button>
              </div>
            </div>
          </div>
        )}

        {status.kind === "downloading" && (
          <div>
            <div className="flex items-center justify-between gap-3">
              <div>
                <div className="section-kicker">Downloading</div>
                <p className="mt-1 text-sm font-semibold text-[color:var(--text-strong)]">
                  Pulling the latest release
                </p>
              </div>
              <p className="text-xs font-mono text-[color:var(--text-muted)]">
                {formatBytes(status.downloaded)}
                {status.total ? ` / ${formatBytes(status.total)}` : ""}
              </p>
            </div>
            <div className="mt-4 h-2 rounded-full bg-[color:var(--track-bg)] p-[2px]">
              <div
                className="h-full rounded-full transition-all duration-300"
                style={{
                  background: "var(--track-high)",
                  width:
                    status.total && status.total > 0
                      ? `${Math.min(100, (status.downloaded / status.total) * 100)}%`
                      : "50%",
                }}
              />
            </div>
          </div>
        )}

        {status.kind === "ready" && (
          <div className="flex gap-3">
            <div className="mt-1 flex h-10 w-10 shrink-0 items-center justify-center rounded-2xl border border-[color:var(--success-border)] bg-[color:var(--success-soft)] text-[color:var(--success)]">
              <CheckIcon className="h-5 w-5" />
            </div>
            <div className="min-w-0 flex-1">
              <div className="section-kicker">Ready</div>
              <p className="mt-2 text-sm font-semibold text-[color:var(--text-strong)]">
                Update downloaded. Restart to apply it.
              </p>
              <div className="mt-4 flex gap-2">
                <button
                  onClick={() => setDismissed(true)}
                  className="toolbar-button !min-h-10 justify-center px-3 text-xs"
                >
                  Later
                </button>
                <button
                  onClick={() => {
                    void handleRelaunch();
                  }}
                  className="toolbar-button toolbar-button-primary !min-h-10 justify-center px-3 text-xs"
                >
                  Restart
                </button>
              </div>
            </div>
          </div>
        )}

        {status.kind === "error" && (
          <div className="flex gap-3">
            <div className="mt-1 flex h-10 w-10 shrink-0 items-center justify-center rounded-2xl border border-[color:var(--danger-border)] bg-[color:var(--danger-soft)] text-[color:var(--danger)]">
              <CloseIcon className="h-5 w-5" />
            </div>
            <div className="min-w-0 flex-1">
              <div className="section-kicker">Updater error</div>
              <p className="mt-2 text-sm leading-6 text-[color:var(--danger)]">
                {status.message}
              </p>
              <button
                onClick={() => setDismissed(true)}
                className="toolbar-button mt-4 !min-h-10 justify-center px-3 text-xs"
              >
                Dismiss
              </button>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
