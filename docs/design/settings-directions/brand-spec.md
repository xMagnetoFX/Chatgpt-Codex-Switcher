# ChatGPT Codex Switcher design specification

## Verified assets

- App logo: `../../../public/app-logo.svg`
- Native icon source: `../../../src-tauri/icons/logo.svg`

## Existing visual language

- Display and body type: Segoe UI Variable Text, Segoe UI Variable, Segoe UI.
- Monospace type: Cascadia Mono or Cascadia Code.
- Dark foundation: `#181818`, `#202020`, `#252525`, `#2d2d2d`.
- Light foundation: `#e9e9e9`, `#f3f3f3`, `#fbfbfb`, `#ffffff`.
- Fluent interaction blue: `#60cdff` in dark mode and `#0067c0` in light mode.
- Codex brand violet: `#b27aff` in dark mode and `#7446b8` in light mode.
- Semantic colors: green for safe, amber for busy, red for destructive or failed states.
- Window chrome: custom 42 pixel title bar with Windows minimize, maximize, and close controls.

## Design constraints

- Preserve the existing global sidebar and app identity so Settings still belongs to the same product.
- Make the content read as desktop preferences, not a responsive card dashboard.
- Prefer grouped rows, property panes, split views, toolbars, and persistent status regions.
- Use compact desktop density while retaining clear targets and keyboard-visible focus.
- Do not add decorative metrics, fabricated features, gradients, glass effects, or filler settings.
- Every direction must contain all eight real actions and the process-safety state.
- Segoe UI is retained despite being a common font because it is both the current product font and the correct Windows-native choice.

