import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { AddAccountModal } from "./AddAccountModal";

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((resolver) => {
    resolve = resolver;
  });
  return { promise, resolve };
}

function renderModal(overrides: Partial<React.ComponentProps<typeof AddAccountModal>> = {}) {
  const props: React.ComponentProps<typeof AddAccountModal> = {
    isOpen: true,
    onClose: vi.fn(),
    onImportFile: vi.fn().mockResolvedValue(undefined),
    onStartOAuth: vi.fn().mockResolvedValue({ auth_url: "https://example.com/login" }),
    onCompleteOAuth: vi.fn().mockResolvedValue(undefined),
    onCancelOAuth: vi.fn().mockResolvedValue(undefined),
    ...overrides,
  };

  return { ...render(<AddAccountModal {...props} />), props };
}

describe("AddAccountModal", () => {
  it("cancels OAuth startup when the modal closes before a login link is ready", () => {
    const startup = deferred<{ auth_url: string }>();
    const onCancelOAuth = vi.fn().mockResolvedValue(undefined);
    const onClose = vi.fn();
    renderModal({
      onStartOAuth: vi.fn().mockReturnValue(startup.promise),
      onCancelOAuth,
      onClose,
    });

    fireEvent.click(screen.getByRole("button", { name: "Generate login link" }));
    fireEvent.click(screen.getByRole("button", { name: "Close" }));

    expect(onCancelOAuth).toHaveBeenCalledOnce();
    expect(onClose).toHaveBeenCalledOnce();
  });

  it("cancels OAuth startup when switching to file import", () => {
    const startup = deferred<{ auth_url: string }>();
    const onCancelOAuth = vi.fn().mockResolvedValue(undefined);
    renderModal({
      onStartOAuth: vi.fn().mockReturnValue(startup.promise),
      onCancelOAuth,
    });

    fireEvent.click(screen.getByRole("button", { name: "Generate login link" }));
    fireEvent.click(screen.getByRole("button", { name: "Import file" }));

    expect(onCancelOAuth).toHaveBeenCalledOnce();
    expect(screen.getByText("Select auth.json file")).toBeInTheDocument();
  });
});
