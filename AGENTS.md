# Agent Notes

This is a small Tauri v2 app with a static frontend. Prefer scoped changes and avoid committing generated artifacts.

## Useful Commands

```powershell
npm run check
cd src-tauri
cargo check
npm run build
```

## Important Files

- `src/renderer.js` owns the main clipboard manager UI, settings interactions, attention anchors, and clipboard history rendering.
- `src/styles.css` owns all visual layout, including mirrored mode and anchor/history column placement.
- `src/api.js` is the frontend command bridge to Tauri.
- `src-tauri/src/lib.rs` owns clipboard polling, image cache/default-image persistence, settings serialization, window state, and image viewer windows.

## Hygiene

- Do not commit `node_modules/`, `src-tauri/target/`, `src-tauri/gen/`, installers, or release executables.
- Keep user-specific paths, local app data, cache files, and generated bundles out of commits.
- Run `npm run check` and `cargo check` after behavior changes.
