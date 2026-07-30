import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { AccountInfo } from "../types";
import { useAccounts } from "./useAccounts";

const { invokeBackendMock } = vi.hoisted(() => ({
  invokeBackendMock: vi.fn(),
}));

vi.mock("../lib/platform", async () => {
  const actual = await vi.importActual<typeof import("../lib/platform")>("../lib/platform");
  return {
    ...actual,
    invokeBackend: invokeBackendMock,
  };
});

function account(id: string, isActive: boolean): AccountInfo {
  return {
    id,
    name: id,
    email: `${id}@example.com`,
    plan_type: "plus",
    auth_mode: "chat_gpt",
    is_active: isActive,
    created_at: "2026-07-30T00:00:00Z",
    last_used_at: null,
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

beforeEach(() => {
  vi.clearAllMocks();
  invokeBackendMock.mockImplementation((command: string) => {
    if (command === "list_accounts") return Promise.resolve([]);
    if (command === "switch_account") return Promise.resolve(undefined);
    throw new Error(`Unexpected command: ${command}`);
  });
});

describe("useAccounts", () => {
  it("keeps the newest account-list response when requests finish out of order", async () => {
    const { result, unmount } = renderHook(() => useAccounts());
    await waitFor(() => expect(result.current.loading).toBe(false));

    const older = deferred<AccountInfo[]>();
    const newer = deferred<AccountInfo[]>();
    invokeBackendMock
      .mockImplementationOnce(() => older.promise)
      .mockImplementationOnce(() => newer.promise);

    let olderLoad!: Promise<AccountInfo[]>;
    let newerLoad!: Promise<AccountInfo[]>;
    act(() => {
      olderLoad = result.current.loadAccounts();
      newerLoad = result.current.loadAccounts();
    });
    newer.resolve([account("newer", true)]);
    await act(async () => {
      await newerLoad;
    });
    older.resolve([account("older", true)]);
    await act(async () => {
      await olderLoad;
    });

    expect(result.current.accounts.map((entry) => entry.id)).toEqual(["newer"]);
    unmount();
  });

  it("does not report a completed switch as failed when only the reload fails", async () => {
    const { result, unmount } = renderHook(() => useAccounts());
    await waitFor(() => expect(result.current.loading).toBe(false));
    const reloadError = new Error("reload failed");
    invokeBackendMock
      .mockImplementationOnce(() => Promise.resolve(undefined))
      .mockImplementationOnce(() => Promise.reject(reloadError));

    await act(async () => {
      await result.current.switchAccount("target");
    });

    expect(result.current.error).toBe("reload failed");
    unmount();
  });

  it("reloads state after a backend mutation reports partial failure", async () => {
    const { result, unmount } = renderHook(() => useAccounts());
    await waitFor(() => expect(result.current.loading).toBe(false));
    const mutationError = new Error("saved, but activation failed");
    invokeBackendMock
      .mockImplementationOnce(() => Promise.reject(mutationError))
      .mockImplementationOnce(() => Promise.resolve([account("saved", false)]));

    let caught: unknown;
    await act(async () => {
      try {
        await result.current.switchAccount("saved");
      } catch (error) {
        caught = error;
      }
    });

    expect(caught).toBe(mutationError);
    expect(result.current.accounts.map((entry) => entry.id)).toEqual(["saved"]);
    unmount();
  });
});
