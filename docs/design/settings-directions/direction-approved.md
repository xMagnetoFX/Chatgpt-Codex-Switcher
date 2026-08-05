# Approved Settings direction

## User approval

> "Direction A but I dont want [Image #1] one single settings page contains all options"

Follow-up authorization:

> "Continue"

## Selected source

- Direction: A, Native Settings List.
- Direction prototype: `direction-a-native-list.html`
- Direction screenshot: `direction-a-native-list.png`
- User markup reference: the secondary Settings category rail shown in the conversation screenshot.

## Required adaptation

- Remove the secondary Settings category rail completely.
- Keep the existing global application sidebar as the only navigation rail.
- Present every Settings option on one continuous page.
- Preserve all four preferences and all four transfer and backup actions.
- Keep process status in the application sidebar instead of duplicating it in Settings.
- Preserve existing callbacks, persistence, loading states, and accessibility names.
- Do not change Home or other application surfaces.

## Implemented result

- Production rendering: `implemented-single-page.png`
- The existing application sidebar is the only navigation rail.
- All preferences, transfer actions, and backup actions remain visible on one page.

## Follow-up refinement

> "have similar thing at left pannel no need of it..also texts in settings really feels small"

- Remove the duplicate Process safety strip from Settings.
- Increase the Settings type and control scale while keeping the approved one-page structure.
