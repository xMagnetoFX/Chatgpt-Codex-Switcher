import { fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { AccountWithUsage, WarmupSummary } from "./types";

const {
  useAccountsMock,
  invokeBackendMock,
  exportFullBackupFileMock,
  importFullBackupFileMock,
  isTauriRuntimeMock,
  getCurrentWindowMock,
  mockWindow,
} = vi.hoisted(() => ({
  useAccountsMock: vi.fn(),
  invokeBackendMock: vi.fn(),
  exportFullBackupFileMock: vi.fn(),
  importFullBackupFileMock: vi.fn(),
  isTauriRuntimeMock: vi.fn(),
  getCurrentWindowMock: vi.fn(),
  mockWindow: {
    startDragging: vi.fn(),
    toggleMaximize: vi.fn(),
    isMaximized: vi.fn().mockResolvedValue(false),
    innerSize: vi.fn().mockResolvedValue({
      toLogical: vi.fn().mockReturnValue({ width: 1290, height: 860 }),
    }),
    scaleFactor: vi.fn().mockResolvedValue(1.25),
    setSize: vi.fn().mockResolvedValue(undefined),
    onResized: vi.fn().mockResolvedValue(() => {}),
    minimize: vi.fn(),
    close: vi.fn(),
  },
}));

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: getCurrentWindowMock,
  LogicalSize: class LogicalSize {
    constructor(
      public width: number,
      public height: number
    ) {}
  },
}));

vi.mock("./hooks/useAccounts", () => ({
  useAccounts: () => useAccountsMock(),
}));

vi.mock("./lib/platform", async () => {
  const actual = await vi.importActual<typeof import("./lib/platform")>("./lib/platform");
  return {
    ...actual,
    invokeBackend: invokeBackendMock,
    exportFullBackupFile: exportFullBackupFileMock,
    importFullBackupFile: importFullBackupFileMock,
    isTauriRuntime: isTauriRuntimeMock,
  };
});

function makeAccount(id: string, name: string, email: string, isActive = false): AccountWithUsage {
  return {
    id,
    name,
    email,
    plan_type: "plus",
    auth_mode: "chat_gpt",
    is_active: isActive,
    created_at: "2026-04-20T00:00:00Z",
    last_used_at: null,
    usageLoading: false,
    usage: {
      account_id: id,
      plan_type: "plus",
      primary_used_percent: 18,
      primary_window_minutes: 300,
      primary_resets_at: Math.floor(Date.now() / 1000) + 7200,
      secondary_used_percent: 36,
      secondary_window_minutes: 10080,
      secondary_resets_at: Math.floor(Date.now() / 1000) + 86400,
      has_credits: true,
      unlimited_credits: false,
      credits_balance: "0",
      banked_resets: 0,
      error: null,
    },
  };
}

function buildUseAccountsReturn(overrides: Partial<ReturnType<typeof baseUseAccountsReturn>> = {}) {
  return {
    ...baseUseAccountsReturn(),
    ...overrides,
  };
}

function baseUseAccountsReturn() {
  return {
    accounts: [makeAccount("active", "Active Pilot", "active@example.com", true)],
    loading: false,
    error: null,
    loadAccounts: vi.fn().mockResolvedValue([]),
    refreshUsage: vi.fn().mockResolvedValue(undefined),
    refreshSingleUsage: vi.fn().mockResolvedValue(undefined),
    warmupAccount: vi.fn().mockResolvedValue(undefined),
    warmupAllAccounts: vi.fn<() => Promise<WarmupSummary>>().mockResolvedValue({
      total_accounts: 1,
      warmed_accounts: 1,
      failed_account_ids: [],
    }),
    switchAccount: vi.fn().mockResolvedValue(undefined),
    restartCodexAndSwitchAccount: vi.fn().mockResolvedValue(undefined),
    deleteAccount: vi.fn().mockResolvedValue(undefined),
    renameAccount: vi.fn().mockResolvedValue(undefined),
    importFromFile: vi.fn().mockResolvedValue(undefined),
    exportAccountsSlimText: vi.fn().mockResolvedValue("payload"),
    importAccountsSlimText: vi.fn().mockResolvedValue({
      imported_count: 1,
      skipped_count: 0,
      total_in_payload: 1,
    }),
    startOAuthLogin: vi
      .fn()
      .mockResolvedValue({ auth_url: "https://example.com", callback_port: 3210 }),
    completeOAuthLogin: vi.fn().mockResolvedValue(undefined),
    cancelOAuthLogin: vi.fn().mockResolvedValue(undefined),
    loadMaskedAccountIds: vi.fn().mockResolvedValue([]),
    saveMaskedAccountIds: vi.fn().mockResolvedValue(undefined),
  };
}

async function renderApp() {
  const { default: App } = await import("./App");
  return render(<App />);
}

async function flushAsyncWork() {
  await Promise.resolve();
  await Promise.resolve();
}

beforeEach(() => {
  vi.resetModules();
  vi.clearAllMocks();
  window.localStorage.clear();
  invokeBackendMock.mockResolvedValue({
    count: 0,
    background_count: 0,
    can_switch: true,
    pids: [],
  });
  getCurrentWindowMock.mockReturnValue(mockWindow);
  isTauriRuntimeMock.mockReturnValue(false);
  mockWindow.isMaximized.mockResolvedValue(false);
  mockWindow.innerSize.mockResolvedValue({
    toLogical: vi.fn().mockReturnValue({ width: 1290, height: 860 }),
  });
  mockWindow.scaleFactor.mockResolvedValue(1.25);
  mockWindow.setSize.mockResolvedValue(undefined);
  mockWindow.toggleMaximize.mockResolvedValue(undefined);
  mockWindow.onResized.mockResolvedValue(() => {});
  exportFullBackupFileMock.mockResolvedValue(true);
  importFullBackupFileMock.mockResolvedValue(null);
  useAccountsMock.mockReturnValue(buildUseAccountsReturn());
});

afterEach(() => {
  vi.useRealTimers();
});

describe("App", () => {
  it("renders in browser mode without requesting a Tauri window handle", async () => {
    await renderApp();

    expect(screen.getByText("Account cockpit")).toBeInTheDocument();
    expect(getCurrentWindowMock).not.toHaveBeenCalled();
  });

  it("uses native maximize and restore for the Windows caption button", async () => {
    isTauriRuntimeMock.mockReturnValue(true);
    mockWindow.isMaximized.mockResolvedValue(false);
    await renderApp();
    await flushAsyncWork();

    mockWindow.isMaximized.mockResolvedValue(true);
    fireEvent.click(screen.getByRole("button", { name: "Maximize" }));
    await flushAsyncWork();

    expect(mockWindow.toggleMaximize).toHaveBeenCalledTimes(1);
    expect(mockWindow.isMaximized).toHaveBeenCalled();
    expect(await screen.findByRole("button", { name: "Restore" })).toBeInTheDocument();
  });

  it("clamps a restored window state to the configured minimum size", async () => {
    isTauriRuntimeMock.mockReturnValue(true);
    mockWindow.innerSize.mockResolvedValue({
      toLogical: vi.fn().mockReturnValue({ width: 1278, height: 850 }),
    });
    await renderApp();
    await flushAsyncWork();

    expect(mockWindow.setSize).toHaveBeenCalledWith(
      expect.objectContaining({ width: 1290, height: 860 })
    );
  });

  it("starts native caption dragging even while the window is maximized", async () => {
    isTauriRuntimeMock.mockReturnValue(true);
    mockWindow.isMaximized.mockResolvedValue(true);
    await renderApp();
    await flushAsyncWork();

    fireEvent.mouseDown(screen.getByTestId("window-drag-region"), {
      button: 0,
      detail: 1,
    });

    expect(mockWindow.startDragging).toHaveBeenCalledTimes(1);
  });

  it("uses title-bar double-click for native maximize without starting a second drag", async () => {
    isTauriRuntimeMock.mockReturnValue(true);
    await renderApp();
    await flushAsyncWork();

    const dragRegion = screen.getByTestId("window-drag-region");
    fireEvent.mouseDown(dragRegion, { button: 0, detail: 2 });
    fireEvent.doubleClick(dragRegion);
    await flushAsyncWork();

    expect(mockWindow.startDragging).not.toHaveBeenCalled();
    expect(mockWindow.toggleMaximize).toHaveBeenCalledTimes(1);
  });

  it("keeps Home focused on refresh while the sidebar remains the single add-account entry point", async () => {
    const refreshUsage = vi.fn().mockResolvedValue(undefined);
    useAccountsMock.mockReturnValue(buildUseAccountsReturn({ refreshUsage }));

    await renderApp();

    expect(screen.getByRole("button", { name: /Refresh account usage/i })).toBeInTheDocument();
    expect(document.querySelector(".app-topbar")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Hide all accounts/i })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Switch to light mode/i })).not.toBeInTheDocument();
    expect(screen.getAllByRole("button", { name: /Add account/i })).toHaveLength(1);

    fireEvent.click(screen.getByRole("button", { name: /Refresh account usage/i }));
    await flushAsyncWork();

    expect(refreshUsage).toHaveBeenCalledTimes(1);
  });

  it("keeps Settings on the shared top bar", async () => {
    await renderApp();

    fireEvent.click(screen.getByRole("button", { name: /Settings/i }));

    expect(document.querySelector(".app-topbar")).toBeInTheDocument();
    expect(screen.getByText("Workspace controls")).toBeInTheDocument();
  });

  it("restarts Codex before switching when the latest process check is locked", async () => {
    const switchAccount = vi.fn().mockResolvedValue(undefined);
    const restartCodexAndSwitchAccount = vi.fn().mockResolvedValue(undefined);
    useAccountsMock.mockReturnValue(
      buildUseAccountsReturn({
        accounts: [
          makeAccount("active", "Active Pilot", "active@example.com", true),
          makeAccount("standby", "Standby Pilot", "standby@example.com"),
        ],
        switchAccount,
        restartCodexAndSwitchAccount,
      })
    );
    invokeBackendMock.mockResolvedValue({
      count: 1,
      background_count: 0,
      can_switch: false,
      pids: [1234],
    });

    await renderApp();

    fireEvent.click(await screen.findByRole("button", { name: "Restart & switch" }));
    await flushAsyncWork();

    expect(restartCodexAndSwitchAccount).toHaveBeenCalledWith("standby");
    expect(switchAccount).not.toHaveBeenCalled();
  });

  it("keeps switching locked when restart switching is disabled", async () => {
    window.localStorage.setItem("codex-switcher-restart-switch", "false");
    const switchAccount = vi.fn().mockResolvedValue(undefined);
    const restartCodexAndSwitchAccount = vi.fn().mockResolvedValue(undefined);
    useAccountsMock.mockReturnValue(
      buildUseAccountsReturn({
        accounts: [
          makeAccount("active", "Active Pilot", "active@example.com", true),
          makeAccount("standby", "Standby Pilot", "standby@example.com"),
        ],
        switchAccount,
        restartCodexAndSwitchAccount,
      })
    );
    invokeBackendMock.mockResolvedValue({
      count: 1,
      background_count: 0,
      can_switch: false,
      pids: [1234],
    });

    await renderApp();

    expect(await screen.findByRole("button", { name: "Codex running" })).toBeDisabled();
    expect(restartCodexAndSwitchAccount).not.toHaveBeenCalled();
    expect(switchAccount).not.toHaveBeenCalled();
  });

  it("persists restart switching preference from Settings", async () => {
    await renderApp();

    fireEvent.click(screen.getByRole("button", { name: /Settings/i }));
    fireEvent.click(screen.getByRole("button", { name: /Restart switching enabled/i }));

    await flushAsyncWork();

    expect(window.localStorage.getItem("codex-switcher-restart-switch")).toBe("false");
  });

  it("runs auto warm-up on launch and hourly when the preference is already enabled", async () => {
    vi.useFakeTimers();
    window.localStorage.setItem("codex-switcher-auto-warmup", "true");
    const warmupAllAccounts = vi.fn<() => Promise<WarmupSummary>>().mockResolvedValue({
      total_accounts: 1,
      warmed_accounts: 1,
      failed_account_ids: [],
    });
    useAccountsMock.mockReturnValue(buildUseAccountsReturn({ warmupAllAccounts }));

    await renderApp();

    await flushAsyncWork();

    expect(warmupAllAccounts).toHaveBeenCalledTimes(1);

    await vi.advanceTimersByTimeAsync(60 * 60 * 1000);
    await flushAsyncWork();

    expect(warmupAllAccounts).toHaveBeenCalledTimes(2);
  });

  it("starts auto warm-up immediately when enabled from Settings and cancels future runs when turned off", async () => {
    vi.useFakeTimers();
    const warmupAllAccounts = vi.fn<() => Promise<WarmupSummary>>().mockResolvedValue({
      total_accounts: 1,
      warmed_accounts: 1,
      failed_account_ids: [],
    });
    useAccountsMock.mockReturnValue(buildUseAccountsReturn({ warmupAllAccounts }));

    await renderApp();

    fireEvent.click(screen.getByRole("button", { name: /Settings/i }));
    fireEvent.click(screen.getByRole("button", { name: /Auto warm-up disabled/i }));

    await flushAsyncWork();

    expect(warmupAllAccounts).toHaveBeenCalledTimes(1);

    fireEvent.click(screen.getByRole("button", { name: /Auto warm-up enabled/i }));
    await vi.advanceTimersByTimeAsync(60 * 60 * 1000);
    await flushAsyncWork();

    expect(warmupAllAccounts).toHaveBeenCalledTimes(1);
  });
});
