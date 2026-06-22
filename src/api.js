'use strict';

// Resolve the Tauri bridge lazily (at call time) rather than once at module load.
// With `withGlobalTauri` it is normally injected before this script runs, but at early
// boot the injection can lag a tick; reading it on demand keeps a transient gap from
// permanently breaking every API call.
function bridge() {
  return window.__TAURI__;
}

function invoke(command, args) {
  const fn = bridge()?.core?.invoke;
  if (!fn) return Promise.reject(new Error(`Tauri API not ready for "${command}"`));
  return fn(command, args);
}

const IMAGE_FILTERS = [
  {
    name: 'Image Files',
    extensions: ['jpg', 'jpeg', 'png', 'gif', 'bmp', 'webp', 'ico'],
  },
  { name: 'All Files', extensions: ['*'] },
];

window.clipboardAPI = {
  isReady: () => !!bridge()?.core?.invoke,
  assetUrl: filePath => {
    const convert = bridge()?.core?.convertFileSrc;
    return convert ? convert(filePath) : '';
  },
  loadSettings: () => invoke('load_settings'),
  saveSettings: settings => invoke('save_settings', { settings }),
  saveWindowState: () => invoke('save_window_state'),
  adjustWindowBorderlessEdges: expand => invoke('adjust_window_borderless_edges', { expand }),
  drainClipboardItems: () => invoke('drain_clipboard_items'),
  onClipboardItemsReady: handler => {
    const listen = bridge()?.event?.listen;
    if (!listen) return Promise.reject(new Error('Tauri event API not ready'));
    return listen('clipboard-items-ready', handler);
  },
  clearHistory: () => invoke('clear_history'),
  loadHiddenHistory: () => invoke('load_hidden_history'),
  saveHiddenHistory: items => invoke('save_hidden_history', { items }),
  copyItem: item => invoke('copy_item_to_clipboard', {
    kind: item.kind,
    text: item.text || null,
    filePath: item.filePath || null,
  }),
  saveImageAsDefault: filePath => invoke('save_image_as_default', { filePath }),
  listDefaultImages: () => invoke('list_default_images'),
  addDefaultImage: filePath => invoke('add_default_image', { filePath }),
  removeDefaultImage: filePath => invoke('remove_default_image', { filePath }),
  pasteDefaultImage: () => invoke('paste_default_image'),
  openImageWindow: (path, cursorX, cursorY, naturalWidth, naturalHeight) => invoke('open_image_window', {
    path,
    cursorX,
    cursorY,
    naturalW: naturalWidth,
    naturalH: naturalHeight,
  }),
  getAssignedImagePath: () => invoke('get_assigned_image_path'),
  pickImage: async () => {
    const dialog = bridge()?.dialog;
    if (!dialog) throw new Error('Dialog API not ready');
    const selected = await dialog.open({
      multiple: false,
      directory: false,
      filters: IMAGE_FILTERS,
      title: 'Add image to pool',
    });
    return Array.isArray(selected) ? selected[0] || null : selected;
  },
  minimize: () => invoke('window_minimize'),
  close: () => invoke('window_close'),
  show: () => invoke('window_show'),
  startWindowDrag: () => invoke('window_start_drag'),
};
