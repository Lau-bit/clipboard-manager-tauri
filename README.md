# Clipboard Manager Tauri

A compact dark Tauri clipboard manager for Windows-focused desktop workflows. It keeps recent text and image clipboard items visible in a grid, provides large visual displayers for inspecting copied content, and includes persistent "attention anchors" for lightweight work direction when there is no immediate task.

## What It Does

- Watches the system clipboard for copied text, bitmap images, and image files.
- Shows up to 18 clipboard items in a dense 3 x 6 grid.
- Opens image items in floating always-on-top viewer windows.
- Provides one or two large displayers for the selected clipboard item, a default image, or a sticky item.
- Supports zooming, panning, checker backgrounds, default image pools, and pasted default images.
- Includes a toggleable attention-anchor column that reserves one clipboard column and shows up to six configurable work anchors.
- Lets each attention anchor use a title, emoji, image, abstract generated shape pattern, or any combination.
- Persists settings, window state, image pool selections, anchor configuration, and generated anchor patterns across sessions.
- Supports mirrored UI layout and optional startup top-bar hiding for compact desktop placement.

## Attention Anchors

Attention anchors are meant as low-friction cognitive scaffolding: persistent small prompts such as "review one rough edge" or "ship a tiny fix" that remain visible beside clipboard history. They can be enabled or disabled from Settings.

When enabled, the anchor column takes the leftmost clipboard column, reducing visible clipboard items from 18 to 12. Disabling anchors restores the full 18-item clipboard grid and restores any temporarily hidden items from the app's hidden-history snapshot.

Each anchor can be configured with:

- active/inactive state
- emoji
- title
- image selected from disk
- image pasted from the clipboard
- generated abstract shape pattern

## Development

Requirements:

- Node.js and npm
- Rust toolchain
- Tauri v2 prerequisites for your platform

Install dependencies:

```powershell
npm install
```

Run checks:

```powershell
npm run check
cd src-tauri
cargo check
```

Run the app in development:

```powershell
npm run dev
```

Build installers:

```powershell
npm run build
```

## Project Structure

- `src/` - frontend HTML, CSS, and JavaScript.
- `src/api.js` - small Tauri bridge wrapper.
- `src/renderer.js` - main clipboard manager UI and state logic.
- `src/image-view.js` - floating image viewer UI logic.
- `src-tauri/src/lib.rs` - Tauri backend, clipboard watcher, settings persistence, image cache, and window behavior.
- `src-tauri/capabilities/` - Tauri permissions.
- `src-tauri/icons/` - application icons.

## Repository Notes

Generated folders and build outputs are intentionally ignored:

- `node_modules/`
- `src-tauri/target/`
- `src-tauri/gen/`
- packaged installers and release executables

The repository includes lockfiles so other agents and humans can reproduce dependency resolution.
