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
- Preserve all four preferences, all four transfer and backup actions, and process-safety status.
- Preserve existing callbacks, persistence, loading states, and accessibility names.
- Do not change Home or other application surfaces.

## Implemented result

- Production rendering: `implemented-single-page.png`
- The existing application sidebar is the only navigation rail.
- All preferences, transfer actions, backup actions, and process status remain visible on one page.
