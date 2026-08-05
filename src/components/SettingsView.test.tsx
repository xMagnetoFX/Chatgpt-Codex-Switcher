import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ComponentProps } from "react";
import { describe, expect, it, vi } from "vitest";
import { SettingsView } from "./SettingsView";

function renderSettings(overrides: Partial<ComponentProps<typeof SettingsView>> = {}) {
  const props: ComponentProps<typeof SettingsView> = {
    allMasked: false,
    autoWarmupEnabled: false,
    restartSwitchEnabled: true,
    themeMode: "dark",
    isAutoWarmupRunning: false,
    isExportingSlim: false,
    isImportingSlim: false,
    isExportingFull: false,
    isImportingFull: false,
    onImportFullFile: vi.fn(),
    onExportSlimText: vi.fn(),
    onImportSlimText: vi.fn(),
    onExportFullFile: vi.fn(),
    onToggleMaskAll: vi.fn(),
    onToggleAutoWarmup: vi.fn(),
    onToggleRestartSwitch: vi.fn(),
    onToggleTheme: vi.fn(),
    ...overrides,
  };

  render(<SettingsView {...props} />);
  return props;
}

describe("SettingsView", () => {
  it("keeps import and export actions wired", async () => {
    const user = userEvent.setup();
    const props = renderSettings();

    await user.click(screen.getByRole("button", { name: /Export slim text/i }));
    await user.click(screen.getByRole("button", { name: /Import slim text/i }));
    await user.click(screen.getByRole("button", { name: /Export full backup/i }));
    await user.click(screen.getByRole("button", { name: /Restore full backup/i }));

    expect(props.onExportSlimText).toHaveBeenCalledTimes(1);
    expect(props.onImportSlimText).toHaveBeenCalledTimes(1);
    expect(props.onExportFullFile).toHaveBeenCalledTimes(1);
    expect(props.onImportFullFile).toHaveBeenCalledTimes(1);
  });

  it("owns privacy, theme, auto warm-up, and restart-switch controls", async () => {
    const user = userEvent.setup();
    const props = renderSettings({
      allMasked: true,
      autoWarmupEnabled: true,
      restartSwitchEnabled: false,
    });

    expect(screen.getByRole("button", { name: /Reveal all accounts/i })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Switch to light mode/i })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Auto warm-up enabled/i })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Restart switching disabled/i })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Warm all accounts/i })).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: /Reveal all accounts/i }));
    await user.click(screen.getByRole("button", { name: /Switch to light mode/i }));
    await user.click(screen.getByRole("button", { name: /Auto warm-up enabled/i }));
    await user.click(screen.getByRole("button", { name: /Restart switching disabled/i }));

    expect(props.onToggleMaskAll).toHaveBeenCalledTimes(1);
    expect(props.onToggleTheme).toHaveBeenCalledTimes(1);
    expect(props.onToggleAutoWarmup).toHaveBeenCalledTimes(1);
    expect(props.onToggleRestartSwitch).toHaveBeenCalledTimes(1);
  });

  it("presents every setting on one page without a secondary category rail", () => {
    renderSettings();

    expect(screen.getByRole("heading", { name: "Workspace" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Account transfer" })).toBeInTheDocument();
    expect(screen.queryByText("Process safety")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "General" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Automation" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Privacy" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Transfer" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Backups" })).not.toBeInTheDocument();
  });
});
