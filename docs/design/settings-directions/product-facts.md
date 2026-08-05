# ChatGPT Codex Switcher product facts

Source: the repository implementation inspected on 2026-08-06.

- Product name: ChatGPT Codex Switcher.
- Current application version: 1.2.5.
- Runtime: native Tauri desktop window with a React interface.
- Default Windows window size: 1280 by 860 pixels.
- Primary navigation: Home and Settings.
- Settings currently owns eight user actions:
  - Hide or reveal account identities.
  - Switch between light and dark appearance.
  - Enable or disable automatic account warm-up.
  - Enable or disable restarting Codex while switching.
  - Export a slim clipboard payload.
  - Import a slim clipboard payload.
  - Export a full encrypted `.cswf` backup.
  - Restore a full encrypted `.cswf` backup.
- Settings also reports live Codex process safety state.
- Appearance, automatic warm-up, and restart switching are stored locally.
- Account masking is stored in the Switcher account catalog.
- The redesign must not change settings semantics, persistence, import formats, process behavior, or backend contracts.

