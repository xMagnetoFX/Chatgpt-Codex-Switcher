import { useState, useEffect, useCallback, useMemo, useRef, type MouseEvent } from "react";
import { getCurrentWindow, LogicalSize } from "@tauri-apps/api/window";
import {
  AddAccountModal,
  AppShell,
  ConfigModal,
  HomeDashboard,
  SettingsView,
  Sidebar,
  Toasts,
  TopBar,
  UpdateChecker,
} from "./components";
import { useAccounts } from "./hooks/useAccounts";
import { needsAttention } from "./lib/accountDisplay";
import {
  exportFullBackupFile,
  importFullBackupFile,
  isTauriRuntime,
  invokeBackend,
} from "./lib/platform";
import type { CodexProcessInfo } from "./types";
import type { AppView, ConfigModalMode, SortMode, ThemeMode, ToastState } from "./types/ui";
import "./App.css";

const THEME_STORAGE_KEY = "codex-switcher-theme";
const AUTO_WARMUP_STORAGE_KEY = "codex-switcher-auto-warmup";
const RESTART_SWITCH_STORAGE_KEY = "codex-switcher-restart-switch";
const AUTO_WARMUP_INTERVAL_MS = 60 * 60 * 1000;
const MIN_WINDOW_WIDTH = 1290;
const MIN_WINDOW_HEIGHT = 860;
const isMacOs =
  typeof navigator !== "undefined" && /(Mac|iPhone|iPod|iPad)/i.test(navigator.userAgent);
type AppWindow = ReturnType<typeof getCurrentWindow>;
let cachedAppWindow: AppWindow | null = null;

function getAppWindow(): AppWindow | null {
  if (!isTauriRuntime()) return null;
  cachedAppWindow ??= getCurrentWindow();
  return cachedAppWindow;
}

function formatWarmupError(err: unknown) {
  if (!err) return "Unknown error";
  if (err instanceof Error && err.message) return err.message;
  if (typeof err === "string") return err;
  try {
    return JSON.stringify(err);
  } catch {
    return "Unknown error";
  }
}

function App() {
  const {
    accounts,
    loading,
    error,
    loadAccounts,
    refreshUsage,
    warmupAllAccounts,
    switchAccount,
    restartCodexAndSwitchAccount,
    deleteAccount,
    renameAccount,
    importFromFile,
    exportAccountsSlimText,
    importAccountsSlimText,
    startOAuthLogin,
    completeOAuthLogin,
    cancelOAuthLogin,
    loadMaskedAccountIds,
    saveMaskedAccountIds,
  } = useAccounts();

  const [activeView, setActiveView] = useState<AppView>("home");
  const [isAddModalOpen, setIsAddModalOpen] = useState(false);
  const [isConfigModalOpen, setIsConfigModalOpen] = useState(false);
  const [configModalMode, setConfigModalMode] = useState<ConfigModalMode>("slim_export");
  const [configPayload, setConfigPayload] = useState("");
  const [configModalError, setConfigModalError] = useState<string | null>(null);
  const [configCopied, setConfigCopied] = useState(false);
  const [switchingId, setSwitchingId] = useState<string | null>(null);
  const [deleteConfirmId, setDeleteConfirmId] = useState<string | null>(null);
  const [processInfo, setProcessInfo] = useState<CodexProcessInfo | null>(null);
  const [isRefreshing, setIsRefreshing] = useState(false);
  const [isExportingSlim, setIsExportingSlim] = useState(false);
  const [isImportingSlim, setIsImportingSlim] = useState(false);
  const [isExportingFull, setIsExportingFull] = useState(false);
  const [isImportingFull, setIsImportingFull] = useState(false);
  const [isWarmingAll, setIsWarmingAll] = useState(false);
  const [refreshSuccess, setRefreshSuccess] = useState(false);
  const [warmupToast, setWarmupToast] = useState<ToastState | null>(null);
  const [maskedAccounts, setMaskedAccounts] = useState<Set<string>>(new Set());
  const [otherAccountsSort, setOtherAccountsSort] = useState<SortMode>("deadline_asc");
  const [isWindowMaximized, setIsWindowMaximized] = useState(false);
  const [themeMode, setThemeMode] = useState<ThemeMode>(() => {
    if (typeof window === "undefined") return "light";
    try {
      const saved = window.localStorage.getItem(THEME_STORAGE_KEY);
      return saved === "dark" ? "dark" : "light";
    } catch {
      return "light";
    }
  });
  const [autoWarmupEnabled, setAutoWarmupEnabled] = useState(() => {
    if (typeof window === "undefined") return false;
    try {
      return window.localStorage.getItem(AUTO_WARMUP_STORAGE_KEY) === "true";
    } catch {
      return false;
    }
  });
  const [restartSwitchEnabled, setRestartSwitchEnabled] = useState(() => {
    if (typeof window === "undefined") return true;
    try {
      return window.localStorage.getItem(RESTART_SWITCH_STORAGE_KEY) !== "false";
    } catch {
      return true;
    }
  });
  const accountsCountRef = useRef(0);
  const switchInFlightRef = useRef(false);
  const isWarmupRunningRef = useRef(false);
  const autoWarmupTriggeredRef = useRef(false);

  const activeAccount = accounts.find((account) => account.is_active);
  const otherAccounts = useMemo(
    () => accounts.filter((account) => !account.is_active),
    [accounts]
  );
  const activeAccountMasked = activeAccount ? maskedAccounts.has(activeAccount.id) : false;
  const hasRunningProcesses = processInfo ? processInfo.count > 0 : false;
  const attentionCount = accounts.filter(needsAttention).length;
  const allMasked =
    accounts.length > 0 && accounts.every((account) => maskedAccounts.has(account.id));

  const handleTitlebarDrag = useCallback((event: MouseEvent<HTMLDivElement>) => {
    const appWindow = getAppWindow();
    if (!appWindow || event.button !== 0 || event.detail > 1) return;
    void appWindow.startDragging();
  }, []);

  const toggleMaximizeMode = useCallback(() => {
    const appWindow = getAppWindow();
    if (!appWindow) return;
    void appWindow
      .toggleMaximize()
      .then(async () => setIsWindowMaximized(await appWindow.isMaximized()))
      .catch((err) => { console.error("Failed to toggle maximize:", err); });
  }, []);

  const handleTitlebarDoubleClick = useCallback(() => {
    toggleMaximizeMode();
  }, [toggleMaximizeMode]);

  const checkProcesses = useCallback(async (): Promise<CodexProcessInfo | null> => {
    try {
      const info = await invokeBackend<CodexProcessInfo>("check_codex_processes");
      setProcessInfo((prev) => {
        if (
          prev &&
          prev.can_switch === info.can_switch &&
          prev.count === info.count &&
          prev.background_count === info.background_count &&
          prev.pids.length === info.pids.length &&
          prev.pids.every((pid, index) => pid === info.pids[index])
        ) {
          return prev;
        }
        return info;
      });
      return info;
    } catch (err) {
      console.error("Failed to check processes:", err);
      return null;
    }
  }, []);

  // The window starts hidden (Windows config) so users never see an
  // unpainted webview flash black; reveal it once React has produced its
  // first frame. A Rust-side fallback shows it anyway after 5s if this
  // never runs.
  useEffect(() => {
    const appWindow = getAppWindow();
    if (!appWindow) return;
    const frame = window.requestAnimationFrame(() => {
      void appWindow
        .show()
        .then(() => appWindow.setFocus())
        .catch((err) => console.error("Failed to show window:", err));
    });
    return () => window.cancelAnimationFrame(frame);
  }, []);

  useEffect(() => {
    void checkProcesses();
    const interval = window.setInterval(() => {
      void checkProcesses();
    }, 5000);
    return () => window.clearInterval(interval);
  }, [checkProcesses]);

  useEffect(() => {
    void loadMaskedAccountIds().then((ids) => {
      if (ids.length > 0) {
        setMaskedAccounts(new Set(ids));
      }
    });
  }, [loadMaskedAccountIds]);

  useEffect(() => {
    const isDark = themeMode === "dark";
    document.documentElement.classList.toggle("dark", isDark);
    document.documentElement.setAttribute("data-theme", themeMode);
    try {
      window.localStorage.setItem(THEME_STORAGE_KEY, themeMode);
    } catch {
      // Ignore storage errors; theme still works for the current session.
    }
  }, [themeMode]);

  useEffect(() => {
    accountsCountRef.current = accounts.length;
  }, [accounts.length]);

  useEffect(() => {
    try {
      window.localStorage.setItem(AUTO_WARMUP_STORAGE_KEY, autoWarmupEnabled ? "true" : "false");
    } catch {
      // Ignore storage errors; auto warm-up still works for the current session.
    }
  }, [autoWarmupEnabled]);

  useEffect(() => {
    try {
      window.localStorage.setItem(
        RESTART_SWITCH_STORAGE_KEY,
        restartSwitchEnabled ? "true" : "false"
      );
    } catch {
      // Ignore storage errors; restart switching still works for the current session.
    }
  }, [restartSwitchEnabled]);

  useEffect(() => {
    if (!isTauriRuntime() || isMacOs) return;

    const appWindow = getAppWindow();
    if (!appWindow) return;

    let unlisten: (() => void) | undefined;

    const syncMaximized = async () => {
      try {
        const maximized = await appWindow.isMaximized();
        setIsWindowMaximized(maximized);
      } catch (err) {
        console.error("Failed to read window state:", err);
      }
    };

    const enforceMinimumWindowSize = async () => {
      try {
        const [physicalSize, scaleFactor] = await Promise.all([
          appWindow.innerSize(),
          appWindow.scaleFactor(),
        ]);
        const logicalSize = physicalSize.toLogical(scaleFactor);
        const width = Math.max(logicalSize.width, MIN_WINDOW_WIDTH);
        const height = Math.max(logicalSize.height, MIN_WINDOW_HEIGHT);
        if (width !== logicalSize.width || height !== logicalSize.height) {
          await appWindow.setSize(new LogicalSize(width, height));
        }
      } catch (err) {
        console.error("Failed to enforce minimum window size:", err);
      }
    };

    void syncMaximized();
    void enforceMinimumWindowSize();

    appWindow
      .onResized(() => { void syncMaximized(); })
      .then((fn) => { unlisten = fn; })
      .catch((err) => { console.error("Failed to watch window resize:", err); });

    return () => { unlisten?.(); };
  }, []);

  const toggleMaskAll = () => {
    setMaskedAccounts((prev) => {
      const shouldMaskAll = !accounts.every((account) => prev.has(account.id));
      const next = shouldMaskAll
        ? new Set(accounts.map((account) => account.id))
        : new Set<string>();
      void saveMaskedAccountIds(Array.from(next));
      return next;
    });
  };

  const showWarmupToast = useCallback((message: string, isError = false) => {
    setWarmupToast({ message, isError });
    window.setTimeout(() => setWarmupToast(null), 2500);
  }, []);

  const handleSwitch = async (accountId: string) => {
    if (switchInFlightRef.current) return;
    switchInFlightRef.current = true;
    setSwitchingId(accountId);

    try {
      const latestProcessInfo = await checkProcesses();
      if (latestProcessInfo && !latestProcessInfo.can_switch && !restartSwitchEnabled) {
        showWarmupToast(
          "Codex is running. Close it first, or enable restart & switch in Settings.",
          true
        );
        return;
      }

      const shouldRestartCodex = latestProcessInfo
        ? !latestProcessInfo.can_switch && restartSwitchEnabled
        : false;
      if (shouldRestartCodex) {
        await restartCodexAndSwitchAccount(accountId);
      } else {
        await switchAccount(accountId);
      }
      void checkProcesses();
    } catch (err) {
      console.error("Failed to switch account:", err);
      showWarmupToast(`Switch failed: ${formatWarmupError(err)}`, true);
    } finally {
      switchInFlightRef.current = false;
      setSwitchingId(null);
    }
  };

  const handleDelete = async (accountId: string) => {
    if (deleteConfirmId !== accountId) {
      setDeleteConfirmId(accountId);
      window.setTimeout(() => setDeleteConfirmId(null), 3000);
      return;
    }

    try {
      await deleteAccount(accountId);
      setDeleteConfirmId(null);
    } catch (err) {
      console.error("Failed to delete account:", err);
    }
  };

  const handleRefresh = async () => {
    setIsRefreshing(true);
    setRefreshSuccess(false);
    try {
      await refreshUsage(undefined, { refreshMetadata: true });
      setRefreshSuccess(true);
      window.setTimeout(() => setRefreshSuccess(false), 2000);
    } catch (err) {
      console.error("Failed to refresh usage:", err);
      showWarmupToast(`Usage refresh failed: ${formatWarmupError(err)}`, true);
    } finally {
      setIsRefreshing(false);
    }
  };

  const handleReloadWorkspace = async () => {
    setIsRefreshing(true);
    setRefreshSuccess(false);
    try {
      const accountList = await loadAccounts();
      await refreshUsage(accountList, { refreshMetadata: true });
      setRefreshSuccess(true);
      window.setTimeout(() => setRefreshSuccess(false), 2000);
    } catch (err) {
      console.error("Failed to reload workspace:", err);
      showWarmupToast(`Workspace reload failed: ${formatWarmupError(err)}`, true);
    } finally {
      setIsRefreshing(false);
    }
  };

  const runWarmupAll = useCallback(
    async ({ automatic = false }: { automatic?: boolean } = {}) => {
      if (accountsCountRef.current === 0 || isWarmupRunningRef.current) {
        return false;
      }

      try {
        isWarmupRunningRef.current = true;
        setIsWarmingAll(true);
        const summary = await warmupAllAccounts();
        if (summary.total_accounts === 0) {
          if (!automatic) {
            showWarmupToast("No accounts available for warm-up", true);
          }
          return false;
        }

        if (summary.failed_account_ids.length === 0) {
          if (!automatic) {
            showWarmupToast(
              `Warm-up sent for all ${summary.warmed_accounts} account${
                summary.warmed_accounts === 1 ? "" : "s"
              }`
            );
          }
        } else {
          showWarmupToast(
            automatic
              ? `Auto warm-up hit ${summary.failed_account_ids.length} failure${
                  summary.failed_account_ids.length === 1 ? "" : "s"
                }.`
              : `Warmed ${summary.warmed_accounts}/${summary.total_accounts}. Failed: ${summary.failed_account_ids.length}`,
            true
          );
        }

        return true;
      } catch (err) {
        console.error("Failed to warm up all accounts:", err);
        showWarmupToast(
          automatic
            ? `Auto warm-up failed: ${formatWarmupError(err)}`
            : `Warm-up all failed: ${formatWarmupError(err)}`,
          true
        );
        return false;
      } finally {
        isWarmupRunningRef.current = false;
        setIsWarmingAll(false);
      }
    },
    [showWarmupToast, warmupAllAccounts]
  );

  useEffect(() => {
    if (!autoWarmupEnabled) {
      autoWarmupTriggeredRef.current = false;
      return;
    }

    if (loading || accounts.length === 0 || autoWarmupTriggeredRef.current) {
      return;
    }

    autoWarmupTriggeredRef.current = true;
    void runWarmupAll({ automatic: true });
  }, [accounts.length, autoWarmupEnabled, loading, runWarmupAll]);

  useEffect(() => {
    if (!autoWarmupEnabled) {
      return;
    }

    const interval = window.setInterval(() => {
      void runWarmupAll({ automatic: true });
    }, AUTO_WARMUP_INTERVAL_MS);

    return () => {
      window.clearInterval(interval);
    };
  }, [autoWarmupEnabled, runWarmupAll]);

  const handleExportSlimText = async () => {
    setConfigModalMode("slim_export");
    setConfigModalError(null);
    setConfigPayload("");
    setConfigCopied(false);
    setIsConfigModalOpen(true);

    try {
      setIsExportingSlim(true);
      const payload = await exportAccountsSlimText();
      setConfigPayload(payload);
      showWarmupToast(`Slim text exported (${accounts.length} accounts).`);
    } catch (err) {
      console.error("Failed to export slim text:", err);
      const message = err instanceof Error ? err.message : String(err);
      setConfigModalError(message);
      showWarmupToast("Slim export failed", true);
    } finally {
      setIsExportingSlim(false);
    }
  };

  const openImportSlimTextModal = () => {
    setConfigModalMode("slim_import");
    setConfigModalError(null);
    setConfigPayload("");
    setConfigCopied(false);
    setIsConfigModalOpen(true);
  };

  const handleImportSlimText = async () => {
    if (!configPayload.trim()) {
      setConfigModalError("Please paste the slim text string first.");
      return;
    }

    try {
      setIsImportingSlim(true);
      setConfigModalError(null);
      const summary = await importAccountsSlimText(configPayload);
      setIsConfigModalOpen(false);
      showWarmupToast(
        `Imported ${summary.imported_count}, skipped ${summary.skipped_count} (total ${summary.total_in_payload})`
      );
    } catch (err) {
      console.error("Failed to import slim text:", err);
      const message = err instanceof Error ? err.message : String(err);
      setConfigModalError(message);
      showWarmupToast("Slim import failed", true);
    } finally {
      setIsImportingSlim(false);
    }
  };

  const handleExportFullFile = async () => {
    try {
      setIsExportingFull(true);
      const exported = await exportFullBackupFile();
      if (!exported) return;
      showWarmupToast("Full encrypted file exported.");
    } catch (err) {
      console.error("Failed to export full encrypted file:", err);
      showWarmupToast("Full export failed", true);
    } finally {
      setIsExportingFull(false);
    }
  };

  const handleImportFullFile = async () => {
    try {
      setIsImportingFull(true);
      const summary = await importFullBackupFile();
      if (!summary) return;
      const accountList = await loadAccounts();
      await refreshUsage(accountList, { refreshMetadata: true });
      const maskedIds = await loadMaskedAccountIds();
      setMaskedAccounts(new Set(maskedIds));
      showWarmupToast(
        `Imported ${summary.imported_count}, skipped ${summary.skipped_count} (total ${summary.total_in_payload})`
      );
    } catch (err) {
      console.error("Failed to import full encrypted file:", err);
      showWarmupToast("Full import failed", true);
    } finally {
      setIsImportingFull(false);
    }
  };

  const sortedOtherAccounts = useMemo(() => {
    const getResetDeadline = (resetAt: number | null | undefined) =>
      resetAt ?? Number.POSITIVE_INFINITY;

    const getRemainingPercent = (usedPercent: number | null | undefined) => {
      if (usedPercent === null || usedPercent === undefined) {
        return Number.NEGATIVE_INFINITY;
      }
      return Math.max(0, 100 - usedPercent);
    };

    return [...otherAccounts].sort((a, b) => {
      if (otherAccountsSort === "deadline_asc" || otherAccountsSort === "deadline_desc") {
        const deadlineDiff =
          getResetDeadline(a.usage?.primary_resets_at) -
          getResetDeadline(b.usage?.primary_resets_at);
        if (deadlineDiff !== 0) {
          return otherAccountsSort === "deadline_asc" ? deadlineDiff : -deadlineDiff;
        }
        const remainingDiff =
          getRemainingPercent(b.usage?.primary_used_percent) -
          getRemainingPercent(a.usage?.primary_used_percent);
        if (remainingDiff !== 0) return remainingDiff;
        return a.name.localeCompare(b.name);
      }

      const remainingDiff =
        getRemainingPercent(b.usage?.primary_used_percent) -
        getRemainingPercent(a.usage?.primary_used_percent);
      if (otherAccountsSort === "remaining_desc" && remainingDiff !== 0) {
        return remainingDiff;
      }
      if (otherAccountsSort === "remaining_asc" && remainingDiff !== 0) {
        return -remainingDiff;
      }
      const deadlineDiff =
        getResetDeadline(a.usage?.primary_resets_at) - getResetDeadline(b.usage?.primary_resets_at);
      if (deadlineDiff !== 0) return deadlineDiff;
      return a.name.localeCompare(b.name);
    });
  }, [otherAccounts, otherAccountsSort]);

  const pageMeta =
    activeView === "home"
      ? {
          title: "Home",
          heading: "Account cockpit",
          description:
            "Scan account state, refresh usage, and switch with an optional Codex restart when needed.",
        }
      : {
          title: "Settings",
          heading: "Workspace controls",
          description: "Handle imports, backups, privacy, appearance, and background automation.",
        };

  return (
    <>
      <AppShell
        isMacOs={isMacOs}
        isWindowMaximized={isWindowMaximized}
        onTitlebarDrag={handleTitlebarDrag}
        onTitlebarDoubleClick={handleTitlebarDoubleClick}
        onMinimize={() => {
          void getAppWindow()?.minimize();
        }}
        onToggleMaximize={toggleMaximizeMode}
        onClose={() => {
          void getAppWindow()?.close();
        }}
        sidebar={
          <Sidebar
            activeView={activeView}
            processInfo={processInfo}
            hasRunningProcesses={hasRunningProcesses}
            onViewChange={setActiveView}
            onAddAccount={() => setIsAddModalOpen(true)}
          />
        }
        topBar={
          activeView === "settings" ? (
            <TopBar
              title={pageMeta.title}
              heading={pageMeta.heading}
              description={pageMeta.description}
              compact
            />
          ) : null
        }
      >
        {activeView === "home" ? (
          <HomeDashboard
            title={pageMeta.title}
            heading={pageMeta.heading}
            description={pageMeta.description}
            accounts={accounts}
            activeAccount={activeAccount}
            otherAccounts={otherAccounts}
            sortedOtherAccounts={sortedOtherAccounts}
            loading={loading}
            error={error}
            processInfo={processInfo}
            hasRunningProcesses={hasRunningProcesses}
            activeAccountMasked={activeAccountMasked}
            attentionCount={attentionCount}
            switchingId={switchingId}
            isRefreshing={isRefreshing}
            isWindowMaximized={isWindowMaximized}
            allMasked={allMasked}
            restartSwitchEnabled={restartSwitchEnabled}
            maskedAccounts={maskedAccounts}
            otherAccountsSort={otherAccountsSort}
            onSortChange={setOtherAccountsSort}
            onRefresh={() => {
              void handleRefresh();
            }}
            onReloadWorkspace={() => {
              void handleReloadWorkspace();
            }}
            onSwitch={(accountId) => {
              void handleSwitch(accountId);
            }}
            onDelete={(accountId) => {
              void handleDelete(accountId);
            }}
            onRename={renameAccount}
            onToggleMaskAll={toggleMaskAll}
          />
        ) : (
          <SettingsView
            allMasked={allMasked}
            autoWarmupEnabled={autoWarmupEnabled}
            restartSwitchEnabled={restartSwitchEnabled}
            themeMode={themeMode}
            isAutoWarmupRunning={isWarmingAll}
            isExportingSlim={isExportingSlim}
            isImportingSlim={isImportingSlim}
            isExportingFull={isExportingFull}
            isImportingFull={isImportingFull}
            onImportFullFile={() => {
              void handleImportFullFile();
            }}
            onExportSlimText={() => {
              void handleExportSlimText();
            }}
            onImportSlimText={openImportSlimTextModal}
            onExportFullFile={() => {
              void handleExportFullFile();
            }}
            onToggleMaskAll={toggleMaskAll}
            onToggleAutoWarmup={() => {
              setAutoWarmupEnabled((prev) => !prev);
            }}
            onToggleRestartSwitch={() => {
              setRestartSwitchEnabled((prev) => !prev);
            }}
            onToggleTheme={() => setThemeMode((prev) => (prev === "dark" ? "light" : "dark"))}
          />
        )}
      </AppShell>

      <Toasts
        refreshSuccess={refreshSuccess}
        warmupToast={warmupToast}
        deleteConfirmId={deleteConfirmId}
      />

      <AddAccountModal
        isOpen={isAddModalOpen}
        onClose={() => setIsAddModalOpen(false)}
        onImportFile={importFromFile}
        onStartOAuth={startOAuthLogin}
        onCompleteOAuth={completeOAuthLogin}
        onCancelOAuth={cancelOAuthLogin}
      />

      {isConfigModalOpen && (
        <ConfigModal
          mode={configModalMode}
          payload={configPayload}
          error={configModalError}
          copied={configCopied}
          isExportingSlim={isExportingSlim}
          isImportingSlim={isImportingSlim}
          onPayloadChange={setConfigPayload}
          onClose={() => setIsConfigModalOpen(false)}
          onCopy={() => {
            if (!configPayload) return;
            void navigator.clipboard
              .writeText(configPayload)
              .then(() => {
                setConfigCopied(true);
                window.setTimeout(() => setConfigCopied(false), 1500);
              })
              .catch(() => {
                setConfigModalError("Clipboard unavailable. Please copy manually.");
              });
          }}
          onImport={() => {
            void handleImportSlimText();
          }}
        />
      )}

      <UpdateChecker />
    </>
  );
}

export default App;
