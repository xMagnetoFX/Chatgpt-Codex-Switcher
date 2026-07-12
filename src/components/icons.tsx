import type { SVGProps } from "react";
import { Add24Regular } from "@fluentui/react-icons/lib/atoms/svg/add";
import { ArrowClockwise24Regular } from "@fluentui/react-icons/lib/atoms/svg/arrow-clockwise";
import { ArrowDownload24Regular } from "@fluentui/react-icons/lib/atoms/svg/arrow-download";
import { ArrowUp24Regular } from "@fluentui/react-icons/lib/atoms/svg/arrow-up";
import { ArrowUpload24Regular } from "@fluentui/react-icons/lib/atoms/svg/arrow-upload";
import { ArrowSwap24Regular } from "@fluentui/react-icons/lib/atoms/svg/arrow-swap";
import { Checkmark24Regular } from "@fluentui/react-icons/lib/atoms/svg/checkmark";
import { ChevronDown24Regular } from "@fluentui/react-icons/lib/atoms/svg/chevron-down";
import { ChevronRight24Regular } from "@fluentui/react-icons/lib/atoms/svg/chevron-right";
import { Copy24Regular } from "@fluentui/react-icons/lib/atoms/svg/copy";
import { Delete24Regular } from "@fluentui/react-icons/lib/atoms/svg/delete";
import { Dismiss24Regular } from "@fluentui/react-icons/lib/atoms/svg/dismiss";
import { Eye24Regular } from "@fluentui/react-icons/lib/atoms/svg/eye";
import { EyeOff24Regular } from "@fluentui/react-icons/lib/atoms/svg/eye-off";
import { Flash24Regular } from "@fluentui/react-icons/lib/atoms/svg/flash";
import { FullScreenMaximize24Regular } from "@fluentui/react-icons/lib/atoms/svg/full-screen-maximize";
import { FullScreenMinimize24Regular } from "@fluentui/react-icons/lib/atoms/svg/full-screen-minimize";
import { Home24Regular } from "@fluentui/react-icons/lib/atoms/svg/home";
import { Open24Regular } from "@fluentui/react-icons/lib/atoms/svg/open";
import { OpenFolder24Regular } from "@fluentui/react-icons/lib/atoms/svg/open-folder";
import { People24Regular } from "@fluentui/react-icons/lib/atoms/svg/people";
import { Person24Regular } from "@fluentui/react-icons/lib/atoms/svg/person";
import { Settings24Regular } from "@fluentui/react-icons/lib/atoms/svg/settings";
import { ShieldCheckmark24Regular } from "@fluentui/react-icons/lib/atoms/svg/shield-checkmark";
import { Square24Regular } from "@fluentui/react-icons/lib/atoms/svg/square";
import { Subtract24Regular } from "@fluentui/react-icons/lib/atoms/svg/subtract";
import { WeatherMoon24Regular } from "@fluentui/react-icons/lib/atoms/svg/weather-moon";
import { WeatherSunny24Regular } from "@fluentui/react-icons/lib/atoms/svg/weather-sunny";
import { Warning24Regular } from "@fluentui/react-icons/lib/atoms/svg/warning";

export type IconProps = SVGProps<SVGSVGElement>;

// Microsoft Fluent System Icons, exposed through the app's existing icon API.
export const HomeIcon = Home24Regular;
export const SettingsIcon = Settings24Regular;
export const EyeIcon = Eye24Regular;
export const EyeOffIcon = EyeOff24Regular;
export const RefreshIcon = ArrowClockwise24Regular;
export const BoltIcon = Flash24Regular;
export const SunIcon = WeatherSunny24Regular;
export const MoonIcon = WeatherMoon24Regular;
export const PlusIcon = Add24Regular;
export const ArchiveUpIcon = ArrowUpload24Regular;
export const ArchiveDownIcon = ArrowDownload24Regular;
export const ShieldIcon = ShieldCheckmark24Regular;
export const ChevronDownIcon = ChevronDown24Regular;
export const ChevronRightIcon = ChevronRight24Regular;
export const MinimizeIcon = Subtract24Regular;
export const MaximizeIcon = Square24Regular;
export const FullscreenIcon = FullScreenMaximize24Regular;
export const RestoreIcon = FullScreenMinimize24Regular;
export const CloseIcon = Dismiss24Regular;
export const TrashIcon = Delete24Regular;
export const CopyIcon = Copy24Regular;
export const ExternalIcon = Open24Regular;
export const FolderIcon = OpenFolder24Regular;
export const ArrowUpIcon = ArrowUp24Regular;
export const CheckIcon = Checkmark24Regular;
export const AccountsIcon = People24Regular;
export const PersonIcon = Person24Regular;
export const SwitchIcon = ArrowSwap24Regular;
export const WarningIcon = Warning24Regular;

// Native caption controls intentionally keep Windows' exact 46x32 geometry.
export function WindowMinimizeIcon(props: IconProps) {
  return (
    <svg viewBox="0 0 46 32" fill="none" stroke="currentColor" strokeWidth="1" {...props}>
      <path d="M18 16.5h10" />
    </svg>
  );
}

export function WindowMaximizeIcon(props: IconProps) {
  return (
    <svg viewBox="0 0 46 32" fill="none" stroke="currentColor" strokeWidth="1" {...props}>
      <rect x="18.5" y="10.5" width="9" height="9" />
    </svg>
  );
}

export function WindowRestoreIcon(props: IconProps) {
  return (
    <svg viewBox="0 0 46 32" fill="none" stroke="currentColor" strokeWidth="1" {...props}>
      <path d="M17.5 13.5h7v7h-7z" />
      <path d="M21.5 9.5h7v7h-4" />
    </svg>
  );
}

export function WindowCloseIcon(props: IconProps) {
  return (
    <svg viewBox="0 0 46 32" fill="none" stroke="currentColor" strokeWidth="1" {...props}>
      <path d="m18.5 10.5 9 9M27.5 10.5l-9 9" />
    </svg>
  );
}
