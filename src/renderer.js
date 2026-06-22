'use strict';

const HISTORY_LIMIT = 18;
const ZOOM_STEP = 1.1;
const MIN_ZOOM = 0.1;
const MAX_ZOOM = 12;
const BUNDLED_DEFAULT_IMAGE = 'pyramid-source.png';
const CLIPBOARD_FALLBACK_POLL_MS = 5000;
// Number of distinct abstract-shape styles to cycle tiles through. Anchors themselves
// are unlimited; this only controls the visual variety of the generated shapes.
const ANCHOR_SHAPE_VARIANTS = 6;
const DEFAULT_ANCHOR_COUNT = 6;
// Anchors are unlimited, but only this many may be active (shown in the column) at once.
const MAX_ACTIVE_ANCHORS = 6;
const ANCHORED_HISTORY_LIMIT = 12;

const els = {
  body: document.body,
  titleDrag: document.getElementById('title-drag'),
  hiddenDragStrip: document.getElementById('hidden-drag-strip'),
  toolbar: document.getElementById('toolbar'),
  btnDual: document.getElementById('btn-dual'),
  btnMirror: document.getElementById('btn-mirror'),
  btnTopbar: document.getElementById('btn-topbar'),
  btnSettings: document.getElementById('btn-settings'),
  btnSettingsClose: document.getElementById('btn-settings-close'),
  btnMinimize: document.getElementById('btn-minimize'),
  btnClose: document.getElementById('btn-close'),
  settingsPanel: document.getElementById('settings-panel'),
  settingDualDisplayers: document.getElementById('setting-dual-displayers'),
  settingMirrorUi: document.getElementById('setting-mirror-ui'),
  settingHideTopbarStartup: document.getElementById('setting-hide-topbar-startup'),
  settingRememberWindowPosition: document.getElementById('setting-remember-window-position'),
  settingExpandBorderlessEdges: document.getElementById('setting-expand-borderless-edges'),
  settingAttentionAnchors: document.getElementById('setting-attention-anchors'),
  btnAttentionAnchors: document.getElementById('btn-attention-anchors'),
  displayers: document.getElementById('displayers'),
  historyPanel: document.getElementById('history-panel'),
  historyGrid: document.getElementById('history-grid'),
  anchorColumn: document.getElementById('anchor-column'),
  anchorGrid: document.getElementById('anchor-grid'),
  btnClear: document.getElementById('btn-clear'),
  btnClearAnchors: document.getElementById('btn-clear-anchors'),
  contextMenu: document.getElementById('context-menu'),
  imagePicker: document.getElementById('image-picker'),
  imagePickerPanel: document.getElementById('image-picker-panel'),
  imagePickerGrid: document.getElementById('image-picker-grid'),
  imagePickerTarget: document.getElementById('image-picker-target'),
  btnAddDefaultImage: document.getElementById('btn-add-default-image'),
  btnPasteDefaultImage: document.getElementById('btn-paste-default-image'),
  btnImagePickerClose: document.getElementById('btn-image-picker-close'),
  anchorManager: document.getElementById('anchor-manager'),
  anchorManagerList: document.getElementById('anchor-manager-list'),
  anchorManagerCount: document.getElementById('anchor-manager-count'),
  btnAddAnchor: document.getElementById('btn-add-anchor'),
  btnClearActiveAnchors: document.getElementById('btn-clear-active-anchors'),
  btnAnchorManagerClose: document.getElementById('btn-anchor-manager-close'),
  toast: document.getElementById('toast'),
};

const state = {
  settings: null,
  history: [],
  selectedId: null,
  selectedItemSnapshot: null,
  selectedAnchorId: null,
  focusedAnchorIndex: 0,
  settingsOpen: false,
  imagePickerOpen: false,
  anchorManagerOpen: false,
  imagePickerIndex: 0,
  displayers: [],
  pollTimer: null,
  clipboardUnlisten: null,
  drainInFlight: false,
  saveWindowTimer: null,
  anchorSaveTimer: null,
  hiddenHistorySaveTimer: null,
  toastTimer: null,
};

function defaultAttentionAnchors() {
  return Array.from({ length: DEFAULT_ANCHOR_COUNT }, (_, index) => ({
    id: `anchor-${index + 1}`,
    active: false,
    emoji: '',
    title: '',
    imagePath: null,
    shapePattern: null,
  }));
}

function blankAnchor() {
  return { id: makeAnchorId(), active: false, emoji: '', title: '', imagePath: null, shapePattern: null };
}

function makeAnchorId() {
  const existing = new Set((state.settings?.attentionAnchors || []).map(anchor => anchor.id));
  let id;
  do {
    id = `anchor-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 6)}`;
  } while (existing.has(id));
  return id;
}

function defaultSettings() {
  return {
    mirrorUi: false,
    topbarVisible: true,
    hideTopbarOnStartup: false,
    rememberWindowPosition: true,
    expandBorderlessEdges: false,
    attentionAnchorsEnabled: true,
    attentionAnchors: defaultAttentionAnchors(),
    dualDisplayers: false,
    activeDisplayer: 0,
    maxHistory: HISTORY_LIMIT,
    window: null,
    displayers: [
      {
        mode: 'defaultImage',
        defaultImagePath: null,
        background: 'solid',
        defaultImageZoomToFill: false,
        clipboardImageZoomToFill: false,
      },
      {
        mode: 'clipboard',
        defaultImagePath: null,
        background: 'solid',
        defaultImageZoomToFill: false,
        clipboardImageZoomToFill: false,
      },
    ],
  };
}

function normalizeSettings(settings) {
  const base = defaultSettings();
  const next = { ...base, ...(settings || {}) };
  next.displayers = [0, 1].map(index => ({
    ...base.displayers[index],
    ...(settings?.displayers?.[index] || {}),
  }));
  next.displayers.forEach(displayer => {
    displayer.defaultImageZoomToFill = !!displayer.defaultImageZoomToFill;
    displayer.clipboardImageZoomToFill = !!displayer.clipboardImageZoomToFill;
  });
  next.maxHistory = HISTORY_LIMIT;
  next.activeDisplayer = next.activeDisplayer === 1 ? 1 : 0;
  next.hideTopbarOnStartup = !!next.hideTopbarOnStartup;
  next.rememberWindowPosition = next.rememberWindowPosition !== false;
  next.expandBorderlessEdges = !!next.expandBorderlessEdges;
  next.attentionAnchorsEnabled = next.attentionAnchorsEnabled !== false;
  next.attentionAnchors = normalizeAnchors(settings?.attentionAnchors || base.attentionAnchors);
  return next;
}

function normalizeAnchors(anchors) {
  if (!Array.isArray(anchors) || anchors.length === 0) {
    return defaultAttentionAnchors();
  }
  const seen = new Set();
  return anchors.map((anchor, index) => {
    const item = anchor || {};
    let id = typeof item.id === 'string' && item.id ? item.id : `anchor-${index + 1}`;
    while (seen.has(id)) {
      id = `anchor-${index + 1}-${Math.random().toString(36).slice(2, 6)}`;
    }
    seen.add(id);
    if (isLegacyDefaultAnchor(item, index)) {
      return { id, active: false, emoji: '', title: '', imagePath: null, shapePattern: null };
    }
    return {
      id,
      active: typeof item.active === 'boolean' ? item.active : false,
      emoji: typeof item.emoji === 'string' ? item.emoji.slice(0, 8) : '',
      title: typeof item.title === 'string' ? item.title : '',
      imagePath: typeof item.imagePath === 'string' && item.imagePath ? item.imagePath : null,
      shapePattern: normalizeShapePattern(item.shapePattern),
    };
  });
}

function isLegacyDefaultAnchor(anchor, index) {
  const legacy = [
    { id: 'next-thread', emoji: '🧭', title: 'Open the next code thread' },
    { id: 'rough-edge', emoji: '🔎', title: 'Review one rough edge' },
    { id: 'tiny-fix', emoji: '✅', title: 'Ship a tiny fix' },
    { id: 'write-note', emoji: '📝', title: 'Write the note down' },
    { id: 'unstick-path', emoji: '🛠️', title: 'Refactor a stuck path' },
    { id: 'capture-reference', emoji: '🖼️', title: 'Capture a useful image' },
  ][index];
  if (!legacy) return false;
  return anchor?.id === legacy.id
    && anchor.active !== false
    && anchor.emoji === legacy.emoji
    && anchor.title === legacy.title
    && !anchor.imagePath
    && !anchor.shapePattern;
}

function normalizeShapePattern(pattern) {
  if (!pattern || typeof pattern !== 'object') return null;
  const stringKeys = [
    'bg',
    'a',
    'b',
    'c',
    'p1',
    'p2',
    'p3',
    's1',
    's2',
    'angle',
  ];
  const next = {};
  for (const key of stringKeys) {
    if (typeof pattern[key] !== 'string' || !pattern[key]) return null;
    next[key] = pattern[key];
  }
  return next;
}

function activeDisplayer() {
  return state.displayers[state.settings.activeDisplayer] || state.displayers[0];
}

function itemUrl(item) {
  if (!item?.filePath) return '';
  return window.clipboardAPI.assetUrl(item.filePath);
}

function itemThumbnailUrl(item) {
  return window.clipboardAPI.assetUrl(item.thumbnailPath || item.filePath);
}

function modeLabel(mode) {
  switch (mode) {
    case 'defaultImage': return 'default image';
    case 'sticky': return 'sticky';
    default: return 'clipboard';
  }
}

function showToast(message) {
  clearTimeout(state.toastTimer);
  els.toast.textContent = message;
  els.toast.classList.add('visible');
  state.toastTimer = setTimeout(() => els.toast.classList.remove('visible'), 1300);
}

async function saveSettings() {
  state.settings = await window.clipboardAPI.saveSettings(state.settings);
  state.settings = normalizeSettings(state.settings);
  applySettingsClasses();
}

function validAnchor(anchor) {
  return !!(anchor?.imagePath || anchor?.emoji?.trim() || anchor?.title?.trim());
}

function activeAnchors() {
  if (!state.settings?.attentionAnchorsEnabled) return [];
  return configuredActiveAnchors();
}

function configuredActiveAnchors() {
  return state.settings.attentionAnchors
    .filter(anchor => anchor.active && validAnchor(anchor))
    .slice(0, MAX_ACTIVE_ANCHORS);
}

function anchorColumnVisible() {
  return !!state.settings?.attentionAnchorsEnabled;
}

function visibleHistoryItems() {
  return state.history.slice(0, anchorColumnVisible() ? ANCHORED_HISTORY_LIMIT : HISTORY_LIMIT);
}

function hiddenHistoryItems() {
  return state.history.slice(ANCHORED_HISTORY_LIMIT, HISTORY_LIMIT);
}

function scheduleHiddenHistorySave() {
  clearTimeout(state.hiddenHistorySaveTimer);
  if (!anchorColumnVisible()) return;
  state.hiddenHistorySaveTimer = setTimeout(() => {
    window.clipboardAPI.saveHiddenHistory(hiddenHistoryItems()).catch(() => {});
  }, 250);
}

async function restoreHiddenHistory() {
  let hidden = [];
  try {
    hidden = await window.clipboardAPI.loadHiddenHistory();
  } catch {
    return;
  }
  if (!hidden.length) return;

  const seen = new Set(state.history.map(item => item.fingerprint || item.id));
  const restored = hidden.filter(item => {
    const key = item?.fingerprint || item?.id;
    if (!key || seen.has(key)) return false;
    seen.add(key);
    return true;
  });
  if (!restored.length) return;

  const head = state.history.slice(0, ANCHORED_HISTORY_LIMIT);
  const tail = state.history.slice(ANCHORED_HISTORY_LIMIT);
  state.history = [...head, ...restored, ...tail].slice(0, HISTORY_LIMIT);
}

function scheduleWindowSave() {
  clearTimeout(state.saveWindowTimer);
  state.saveWindowTimer = setTimeout(() => {
    window.clipboardAPI.saveWindowState().catch(() => {});
  }, 450);
}

function saveWindowSoonAfterDrag() {
  window.clipboardAPI.startWindowDrag();
  scheduleWindowSave();
  setTimeout(scheduleWindowSave, 350);
  setTimeout(scheduleWindowSave, 1000);
  setTimeout(scheduleWindowSave, 1800);
}

function applySettingsClasses() {
  els.body.classList.toggle('mirrored', state.settings.mirrorUi);
  els.body.classList.toggle('topbar-hidden', !state.settings.topbarVisible);
  els.body.classList.toggle('dual-displayers', state.settings.dualDisplayers);
  els.body.classList.toggle('anchors-visible', anchorColumnVisible());
  els.btnDual.classList.toggle('active', state.settings.dualDisplayers);
  els.btnMirror.classList.toggle('active', state.settings.mirrorUi);
  els.btnTopbar.classList.toggle('active', !state.settings.topbarVisible);
  els.btnSettings.classList.toggle('active', state.settingsOpen);
  els.settingsPanel.classList.toggle('open', state.settingsOpen);
  els.settingsPanel.setAttribute('aria-hidden', state.settingsOpen ? 'false' : 'true');
  els.settingDualDisplayers.checked = state.settings.dualDisplayers;
  els.settingMirrorUi.checked = state.settings.mirrorUi;
  els.settingHideTopbarStartup.checked = state.settings.hideTopbarOnStartup;
  els.settingRememberWindowPosition.checked = state.settings.rememberWindowPosition;
  els.settingExpandBorderlessEdges.checked = state.settings.expandBorderlessEdges;
  els.settingAttentionAnchors.checked = state.settings.attentionAnchorsEnabled;
}

function createDisplayer(index) {
  const root = document.createElement('article');
  root.className = 'displayer';
  root.tabIndex = 0;
  root.dataset.index = String(index);

  const content = document.createElement('div');
  content.className = 'displayer-content';

  const tools = document.createElement('div');
  tools.className = 'displayer-tools';

  const copy = document.createElement('button');
  copy.className = 'tool-button';
  copy.title = 'Copy displayed item';
  copy.textContent = '⧉';

  const menu = document.createElement('button');
  menu.className = 'tool-button';
  menu.title = 'Displayer settings';
  menu.textContent = '⋯';

  const chip = document.createElement('div');
  chip.className = 'mode-chip';

  tools.append(copy, menu);
  root.append(content, tools, chip);
  els.displayers.append(root);

  const displayer = {
    index,
    root,
    content,
    chip,
    item: null,
    zoom: 1,
    panX: 0,
    panY: 0,
    isPanning: false,
    startX: 0,
    startY: 0,
    basePanX: 0,
    basePanY: 0,
  };

  root.addEventListener('focus', () => setActiveDisplayer(index));
  root.addEventListener('pointerdown', () => setActiveDisplayer(index));
  root.addEventListener('contextmenu', event => openDisplayerMenu(event, index));
  root.addEventListener('wheel', event => zoomDisplayer(event, displayer), { passive: false });
  root.addEventListener('mousedown', event => startPan(event, displayer));
  root.addEventListener('mousemove', event => movePan(event, displayer));
  root.addEventListener('mouseup', () => endPan(displayer));
  root.addEventListener('mouseleave', () => endPan(displayer));
  root.addEventListener('dblclick', () => resetTransform(displayer));
  copy.addEventListener('click', event => {
    event.stopPropagation();
    copyDisplayed(displayer);
  });
  menu.addEventListener('click', event => {
    event.stopPropagation();
    openDisplayerMenu(event, index);
  });

  return displayer;
}

function buildDisplayers() {
  els.displayers.replaceChildren();
  state.displayers = [createDisplayer(0), createDisplayer(1)];
  renderDisplayers();
}

function setActiveDisplayer(index) {
  const next = index === 1 ? 1 : 0;
  const changed = state.settings.activeDisplayer !== next;
  state.settings.activeDisplayer = next;
  state.displayers.forEach(displayer => {
    displayer.root.classList.toggle('active', displayer.index === next);
  });
  // Only persist when the active displayer actually changes; this fires on every
  // focus/pointerdown, so an unconditional save would spam settings.json to disk.
  if (changed) saveSettings().catch(() => {});
}

function visibleDisplayers() {
  return state.settings.dualDisplayers ? state.displayers : [state.displayers[0]];
}

function renderDisplayers() {
  state.displayers.forEach(displayer => {
    displayer.root.hidden = displayer.index === 1 && !state.settings.dualDisplayers;
    displayer.root.classList.toggle('active', displayer.index === state.settings.activeDisplayer);
    renderDisplayer(displayer);
  });
}

function defaultImageItem(displayer) {
  const settings = state.settings.displayers[displayer.index];
  return {
    kind: 'image',
    filePath: settings.defaultImagePath || BUNDLED_DEFAULT_IMAGE,
    thumbnailPath: null,
    bundled: !settings.defaultImagePath,
  };
}

function renderDisplayer(displayer) {
  const settings = state.settings.displayers[displayer.index];
  displayer.root.classList.toggle('checker', settings.background === 'checker');
  displayer.chip.textContent = `${displayer.index + 1}: ${modeLabel(settings.mode)}`;

  if (settings.mode === 'defaultImage') {
    drawItem(displayer, defaultImageItem(displayer), { preserveTransform: false, source: 'defaultImage' });
    return;
  }

  if (!displayer.item) {
    displayer.content.replaceChildren(emptyDisplayer());
    return;
  }

  drawItem(displayer, displayer.item, { preserveTransform: true, source: 'clipboard' });
}

function emptyDisplayer() {
  const empty = document.createElement('div');
  empty.className = 'empty-displayer';
  empty.textContent = '▲';
  return empty;
}

function drawItem(displayer, item, options = {}) {
  if (!options.preserveTransform) resetTransform(displayer, false);
  displayer.item = item;
  displayer.content.replaceChildren();

  if (item.kind === 'text') {
    const text = document.createElement('div');
    text.className = 'display-text';
    text.textContent = item.text || '';
    displayer.content.append(text);
    applyTransform(displayer);
    return;
  }

  if (item.kind === 'image' && item.filePath) {
    const image = document.createElement('img');
    image.className = 'display-image';
    image.classList.toggle('zoom-fill', shouldZoomImageToFill(displayer, item, options.source));
    image.draggable = false;
    image.src = item.bundled ? item.filePath : itemUrl(item);
    displayer.content.append(image);
    applyTransform(displayer);
    return;
  }

  displayer.content.append(emptyDisplayer());
}

function shouldZoomImageToFill(displayer, item, source) {
  if (item.kind !== 'image') return false;
  const settings = state.settings.displayers[displayer.index];
  return source === 'defaultImage'
    ? settings.defaultImageZoomToFill
    : settings.clipboardImageZoomToFill;
}

function applyTransform(displayer) {
  const target = displayer.content.querySelector('.display-image, .display-text');
  if (!target) return;
  const fillBoost = target.classList.contains('zoom-fill') ? ZOOM_STEP : 1;
  const zoom = Math.min(MAX_ZOOM, displayer.zoom * fillBoost);
  target.style.transform = `translate(${displayer.panX}px, ${displayer.panY}px) scale(${zoom})`;
}

function resetTransform(displayer, apply = true) {
  displayer.zoom = 1;
  displayer.panX = 0;
  displayer.panY = 0;
  if (apply) applyTransform(displayer);
}

function zoomDisplayer(event, displayer) {
  if (!displayer.item) return;
  event.preventDefault();
  const factor = event.deltaY < 0 ? ZOOM_STEP : 1 / ZOOM_STEP;
  displayer.zoom = Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, displayer.zoom * factor));
  applyTransform(displayer);
}

function startPan(event, displayer) {
  if (event.button !== 0 || !displayer.item) return;
  displayer.root.focus();
  displayer.isPanning = true;
  displayer.startX = event.clientX;
  displayer.startY = event.clientY;
  displayer.basePanX = displayer.panX;
  displayer.basePanY = displayer.panY;
}

function movePan(event, displayer) {
  if (!displayer.isPanning) return;
  displayer.panX = displayer.basePanX + event.clientX - displayer.startX;
  displayer.panY = displayer.basePanY + event.clientY - displayer.startY;
  applyTransform(displayer);
}

function endPan(displayer) {
  displayer.isPanning = false;
}

function addHistoryItem(item) {
  if (!item?.fingerprint) return;
  if (state.history[0]?.fingerprint === item.fingerprint) return;
  const shouldDisplayNewItem = !state.selectedId && !state.selectedItemSnapshot;
  state.history = state.history.filter(existing => existing.fingerprint !== item.fingerprint);
  state.history.unshift(item);
  state.history.length = Math.min(state.history.length, state.settings.maxHistory);
  if (shouldDisplayNewItem) {
    state.selectedAnchorId = null;
    state.selectedId = item.id;
    state.selectedItemSnapshot = item;
    showItemInClipboardDisplayers(item);
  }
  renderAnchors();
  renderHistory();
}

function showItemInClipboardDisplayers(item) {
  visibleDisplayers().forEach(displayer => {
    if (state.settings.displayers[displayer.index].mode === 'clipboard') {
      drawItem(displayer, item, { preserveTransform: false, source: 'clipboard' });
    }
  });
}

function renderHistory() {
  els.body.classList.toggle('anchors-visible', anchorColumnVisible());
  const visibleItems = visibleHistoryItems();
  els.body.classList.toggle('has-history', visibleItems.length > 0);
  const existingNodes = new Map(
    [...els.historyGrid.children].map(node => [node.dataset.itemId, node])
  );
  const nodes = visibleItems.map(item => {
    const node = existingNodes.get(item.id) || createHistoryNode(item);
    updateHistoryNode(node, item);
    return node;
  });
  els.historyGrid.replaceChildren(...nodes);
  scheduleHiddenHistorySave();
}

function renderAnchors() {
  els.body.classList.toggle('anchors-visible', anchorColumnVisible());
  const fragment = document.createDocumentFragment();
  activeAnchors().forEach((anchor, index) => fragment.append(createAnchorNode(anchor, index)));
  els.anchorGrid.replaceChildren(fragment);
}

function createAnchorNode(anchor, index) {
  const node = document.createElement('button');
  node.className = `anchor-item anchor-shape-${index % ANCHOR_SHAPE_VARIANTS}`;
  node.dataset.anchorId = anchor.id;
  node.classList.toggle('no-image', !anchor.imagePath);
  node.classList.toggle('selected', anchor.id === state.selectedAnchorId);
  applyShapePattern(node, anchor.shapePattern);
  node.title = [anchor.emoji, anchor.title].filter(Boolean).join(' ');
  // Selection is updated in place (see updateAnchorSelectionClasses) rather than by
  // rebuilding the grid, so the tile survives between the two clicks of a double-click.
  // Double-click itself is handled by delegation on #anchor-column (see bindChrome).
  node.addEventListener('click', () => selectAnchor(anchor));

  if (anchor.imagePath) {
    const image = document.createElement('img');
    image.className = 'anchor-image';
    image.draggable = false;
    image.src = window.clipboardAPI.assetUrl(anchor.imagePath);
    node.append(image);
  }

  const content = document.createElement('div');
  content.className = 'anchor-content';

  if (anchor.emoji?.trim()) {
    const emoji = document.createElement('span');
    emoji.className = 'anchor-emoji';
    emoji.textContent = anchor.emoji.trim();
    content.append(emoji);
  }

  if (anchor.title?.trim()) {
    const title = document.createElement('span');
    title.className = 'anchor-title';
    title.textContent = anchor.title.trim();
    content.append(title);
  }

  node.append(content);
  return node;
}

function updateAnchorSelectionClasses() {
  els.anchorGrid.querySelectorAll('.anchor-item').forEach(node => {
    node.classList.toggle('selected', node.dataset.anchorId === state.selectedAnchorId);
  });
}

function applyShapePattern(node, pattern) {
  if (!pattern) return;
  node.style.setProperty('--shape-bg', pattern.bg);
  node.style.setProperty('--shape-a', pattern.a);
  node.style.setProperty('--shape-b', pattern.b);
  node.style.setProperty('--shape-c', pattern.c);
  node.style.setProperty('--shape-p1', pattern.p1);
  node.style.setProperty('--shape-p2', pattern.p2);
  node.style.setProperty('--shape-p3', pattern.p3);
  node.style.setProperty('--shape-s1', pattern.s1);
  node.style.setProperty('--shape-s2', pattern.s2);
  node.style.setProperty('--shape-angle', pattern.angle);
}

function anchorDisplayItem(anchor) {
  if (anchor.imagePath) {
    return {
      id: `anchor-${anchor.id}`,
      kind: 'image',
      filePath: anchor.imagePath,
      thumbnailPath: null,
      fingerprint: `anchor-image:${anchor.id}:${anchor.imagePath}`,
    };
  }

  const text = [anchor.emoji, anchor.title].filter(value => value?.trim()).join(' ').trim();
  return {
    id: `anchor-${anchor.id}`,
    kind: 'text',
    text,
    fingerprint: `anchor-text:${anchor.id}:${text}`,
  };
}

function selectAnchor(anchor) {
  if (!validAnchor(anchor)) return;
  state.selectedAnchorId = anchor.id;
  state.selectedId = null;
  state.selectedItemSnapshot = null;
  visibleDisplayers().forEach(displayer => {
    if (state.settings.displayers[displayer.index].mode === 'clipboard') {
      drawItem(displayer, anchorDisplayItem(anchor), { preserveTransform: false, source: 'clipboard' });
    }
  });
  els.historyPanel.focus();
  updateAnchorSelectionClasses();
  renderHistory();
}

function createHistoryNode(item) {
  const node = document.createElement('button');
  node.className = 'history-item';
  node.dataset.itemId = item.id;
  node.addEventListener('click', () => selectItem(item));
  node.addEventListener('dblclick', event => openItemInViewer(item, event));

  if (item.kind === 'image') {
    const image = document.createElement('img');
    image.className = 'history-thumb';
    image.draggable = false;
    image.decoding = 'async';
    image.loading = 'lazy';
    node.append(image);
  } else {
    const text = document.createElement('div');
    text.className = 'history-text';
    text.textContent = item.text || '';
    node.append(text);
  }

  return node;
}

function updateHistoryNode(node, item) {
  node.classList.toggle('selected', item.id === state.selectedId);
  node.title = item.kind === 'text' ? item.text || '' : `${item.width || '?'} x ${item.height || '?'}`;
  if (item.kind === 'image') {
    const image = node.querySelector('.history-thumb');
    if (image) {
      image.classList.toggle('fill-width-thumb', shouldFillThumbnailWidth(item));
      const src = itemThumbnailUrl(item);
      if (image.dataset.src !== src) {
        image.dataset.src = src;
        image.src = src;
      }
    }
  } else {
    const text = node.querySelector('.history-text');
    if (text && text.textContent !== (item.text || '')) {
      text.textContent = item.text || '';
    }
  }
}

function shouldFillThumbnailWidth(item) {
  if (!item?.width || !item?.height) return false;
  return item.width >= 320 && item.width / item.height >= 0.65;
}

function selectedItem() {
  return state.history.find(item => item.id === state.selectedId) || state.selectedItemSnapshot || null;
}

function selectItem(item) {
  state.selectedAnchorId = null;
  state.selectedId = item.id;
  state.selectedItemSnapshot = item;
  showItemInClipboardDisplayers(item);
  els.historyPanel.focus();
  renderAnchors();
  renderHistory();
}

async function copyAndRaise(item) {
  if (!item) return;
  await window.clipboardAPI.copyItem(item);
  state.history = state.history.filter(existing => existing.id !== item.id);
  state.history.unshift(item);
  state.history.length = Math.min(state.history.length, state.settings.maxHistory);
  state.selectedAnchorId = null;
  state.selectedId = item.id;
  state.selectedItemSnapshot = item;
  renderAnchors();
  renderHistory();
  showToast('Copied');
}

// Double-click: images open in their own floating viewer window; text still copies.
async function openItemInViewer(item, event) {
  if (item.kind !== 'image' || !item.filePath) {
    await copyAndRaise(item);
    return;
  }
  try {
    await window.clipboardAPI.openImageWindow(item.filePath, event.clientX, event.clientY, item.width || 0, item.height || 0);
  } catch (error) {
    console.error(error);
    showToast('Could not open image window');
  }
}

function deleteSelected() {
  const item = selectedItem();
  if (!item) return;
  state.history = state.history.filter(existing => existing.id !== item.id);
  state.selectedId = state.history[0]?.id || null;
  state.selectedItemSnapshot = state.history[0] || null;
  renderHistory();
}

function moveSelection(direction) {
  const items = visibleHistoryItems();
  if (!items.length) return;
  const index = Math.max(0, items.findIndex(item => item.id === state.selectedId));
  const next = Math.min(items.length - 1, Math.max(0, index + direction));
  state.selectedAnchorId = null;
  state.selectedId = items[next].id;
  state.selectedItemSnapshot = items[next];
  showItemInClipboardDisplayers(items[next]);
  renderAnchors();
  renderHistory();
}

async function copyDisplayed(displayer = activeDisplayer()) {
  if (!displayer?.item) return;
  if (displayer.item.bundled) {
    showToast('Bundled default is display-only');
    return;
  }
  await window.clipboardAPI.copyItem(displayer.item);
  showToast('Copied displayer');
}

function openDisplayerMenu(event, index) {
  event.preventDefault();
  setActiveDisplayer(index);
  const settings = state.settings.displayers[index];
  const displayer = state.displayers[index];
  const hasImage = displayer.item?.kind === 'image' && displayer.item.filePath && !displayer.item.bundled;
  const buttons = [
    menuButton('Display clipboard items', settings.mode === 'clipboard', () => setDisplayerMode(index, 'clipboard')),
    menuButton('Default image', settings.mode === 'defaultImage', () => setDisplayerMode(index, 'defaultImage')),
    menuButton('Sticky current item', settings.mode === 'sticky', () => setDisplayerMode(index, 'sticky')),
    separator(),
    menuButton('Checker background', settings.background === 'checker', () => toggleChecker(index)),
    menuButton('Zoom default image to fill', settings.defaultImageZoomToFill, () => toggleDefaultImageZoomToFill(index)),
    menuButton('Zoom clipboard images to fill', settings.clipboardImageZoomToFill, () => toggleClipboardImageZoomToFill(index)),
    separator(),
    menuButton('Save current image as default', false, () => saveCurrentAsDefault(index), !hasImage),
    menuButton('Choose default image...', false, () => openImagePicker(index)),
    menuButton('Copy displayed item', false, () => copyDisplayed(displayer), !displayer.item || displayer.item.bundled),
    menuButton('Clear displayer', false, () => clearDisplayer(index), settings.mode === 'defaultImage'),
  ];

  els.contextMenu.replaceChildren(...buttons);
  els.contextMenu.classList.add('open');
  const menuRect = els.contextMenu.getBoundingClientRect();
  const maxX = window.innerWidth - menuRect.width - 4;
  const maxY = window.innerHeight - menuRect.height - 4;
  els.contextMenu.style.left = `${Math.max(4, Math.min(event.clientX, maxX))}px`;
  els.contextMenu.style.top = `${Math.max(4, Math.min(event.clientY, maxY))}px`;
}

function menuButton(label, checked, action, disabled = false) {
  const button = document.createElement('button');
  button.textContent = `${checked ? '✓ ' : ''}${label}`;
  button.classList.toggle('checked', checked);
  button.disabled = disabled;
  button.addEventListener('click', async event => {
    event.stopPropagation();
    els.contextMenu.classList.remove('open');
    await action();
  });
  return button;
}

function separator() {
  const line = document.createElement('div');
  line.className = 'menu-separator';
  return line;
}

async function setDisplayerMode(index, mode) {
  state.settings.displayers[index].mode = mode;
  if (mode === 'clipboard' && selectedItem()) {
    drawItem(state.displayers[index], selectedItem(), { preserveTransform: false, source: 'clipboard' });
  }
  renderDisplayers();
  await saveSettings();
}

async function toggleChecker(index) {
  const settings = state.settings.displayers[index];
  settings.background = settings.background === 'checker' ? 'solid' : 'checker';
  renderDisplayers();
  await saveSettings();
}

async function toggleDefaultImageZoomToFill(index) {
  const settings = state.settings.displayers[index];
  settings.defaultImageZoomToFill = !settings.defaultImageZoomToFill;
  resetTransform(state.displayers[index], false);
  renderDisplayers();
  await saveSettings();
}

async function toggleClipboardImageZoomToFill(index) {
  const settings = state.settings.displayers[index];
  settings.clipboardImageZoomToFill = !settings.clipboardImageZoomToFill;
  resetTransform(state.displayers[index], false);
  renderDisplayers();
  await saveSettings();
}

// Clear the clipboard selection and empty any displayer showing it. Default-image and
// sticky displayers keep their content.
function deselectAll() {
  state.selectedAnchorId = null;
  state.selectedId = null;
  state.selectedItemSnapshot = null;
  state.displayers.forEach(displayer => {
    if (state.settings.displayers[displayer.index].mode === 'clipboard') {
      displayer.item = null;
      resetTransform(displayer, false);
    }
  });
  renderDisplayers();
  renderAnchors();
  renderHistory();
}

function openImagePicker(index) {
  state.imagePickerIndex = index;
  state.imagePickerOpen = true;
  els.imagePicker.classList.add('open');
  els.imagePicker.setAttribute('aria-hidden', 'false');
  els.imagePickerTarget.textContent = `Displayer ${index + 1}`;
  renderImagePool();
}

function closeImagePicker() {
  state.imagePickerOpen = false;
  els.imagePicker.classList.remove('open');
  els.imagePicker.setAttribute('aria-hidden', 'true');
}

async function renderImagePool() {
  const current = state.settings.displayers[state.imagePickerIndex]?.defaultImagePath || null;
  let pool = [];
  try {
    pool = await window.clipboardAPI.listDefaultImages();
  } catch (error) {
    console.error(error);
  }

  const tiles = [
    // The bundled built-in default is always available and cannot be removed.
    createPoolTile({ path: null, src: BUNDLED_DEFAULT_IMAGE, selected: !current, label: 'Built-in' }),
    ...pool.map(path => createPoolTile({
      path,
      src: window.clipboardAPI.assetUrl(path),
      selected: current === path,
      removable: true,
    })),
    createAddTile(),
  ];
  els.imagePickerGrid.replaceChildren(...tiles);
}

function createPoolTile({ path, src, selected, removable = false, label }) {
  // A div (not a button) so the optional remove button can nest inside it.
  const tile = document.createElement('div');
  tile.className = 'pool-tile';
  tile.classList.toggle('selected', selected);
  tile.addEventListener('click', () => selectPoolImage(path));

  const image = document.createElement('img');
  image.className = 'pool-thumb';
  image.draggable = false;
  image.src = src;
  tile.append(image);

  if (label) {
    const tag = document.createElement('span');
    tag.className = 'pool-tag';
    tag.textContent = label;
    tile.append(tag);
  }

  if (removable) {
    const remove = document.createElement('button');
    remove.className = 'pool-remove';
    remove.title = 'Remove from pool';
    remove.textContent = '×';
    remove.addEventListener('click', event => {
      event.stopPropagation();
      removeFromPool(path);
    });
    tile.append(remove);
  }

  return tile;
}

function createAddTile() {
  const tile = document.createElement('button');
  tile.className = 'pool-tile pool-add';
  tile.title = 'Add image to pool';
  tile.textContent = '+';
  tile.addEventListener('click', addToPool);
  return tile;
}

async function selectPoolImage(path) {
  const index = state.imagePickerIndex;
  state.settings.displayers[index].defaultImagePath = path;
  state.settings.displayers[index].mode = 'defaultImage';
  resetTransform(state.displayers[index], false);
  renderDisplayers();
  await saveSettings();
  closeImagePicker();
}

async function addToPool() {
  let filePath = null;
  try {
    filePath = await window.clipboardAPI.pickImage();
  } catch (error) {
    console.error(error);
    showToast('Could not open file picker');
    return;
  }
  if (!filePath) return;
  try {
    await window.clipboardAPI.addDefaultImage(filePath);
  } catch (error) {
    console.error(error);
    showToast('Could not add image');
    return;
  }
  await renderImagePool();
  showToast('Added to pool');
}

async function pasteToPool() {
  let path = null;
  try {
    path = await window.clipboardAPI.pasteDefaultImage();
  } catch (error) {
    console.error(error);
    showToast('Could not paste image');
    return;
  }
  if (!path) {
    showToast('No image on clipboard');
    return;
  }
  await renderImagePool();
  showToast('Pasted to pool');
}

async function removeFromPool(path) {
  try {
    await window.clipboardAPI.removeDefaultImage(path);
  } catch (error) {
    console.error(error);
    showToast('Could not remove image');
    return;
  }
  // Any displayer using the removed image falls back to the built-in default.
  let changed = false;
  state.settings.displayers.forEach(displayer => {
    if (displayer.defaultImagePath === path) {
      displayer.defaultImagePath = null;
      changed = true;
    }
  });
  if (changed) {
    renderDisplayers();
    await saveSettings();
  }
  await renderImagePool();
}

async function saveCurrentAsDefault(index) {
  const displayer = state.displayers[index];
  if (!displayer.item?.filePath || displayer.item.bundled) return;
  const savedPath = await window.clipboardAPI.saveImageAsDefault(displayer.item.filePath);
  state.settings.displayers[index].defaultImagePath = savedPath;
  state.settings.displayers[index].mode = 'defaultImage';
  renderDisplayers();
  await saveSettings();
  showToast('Default saved');
}

async function clearDisplayer(index) {
  state.displayers[index].item = null;
  resetTransform(state.displayers[index], false);
  renderDisplayers();
}

function openAnchorManager(focusAnchorId = null) {
  reflowAnchorsActiveFirst();
  const anchors = state.settings.attentionAnchors;
  const lastIndex = Math.max(0, anchors.length - 1);
  let focusIndex = state.focusedAnchorIndex;
  if (focusAnchorId) {
    const found = anchors.findIndex(anchor => anchor.id === focusAnchorId);
    if (found >= 0) focusIndex = found;
  }
  state.focusedAnchorIndex = Math.max(0, Math.min(lastIndex, focusIndex || 0));
  state.anchorManagerOpen = true;
  setSettingsOpen(false);
  els.anchorManager.classList.add('open');
  els.anchorManager.setAttribute('aria-hidden', 'false');
  renderAnchorManager();
}

// Float active anchors to the top (stable) so the column order and the manager order
// agree after the menu is re-opened. Persists only when the order actually changes.
function reflowAnchorsActiveFirst() {
  const anchors = state.settings.attentionAnchors;
  const isActive = anchor => anchor.active && validAnchor(anchor);
  const reordered = [...anchors.filter(isActive), ...anchors.filter(anchor => !isActive(anchor))];
  if (reordered.some((anchor, i) => anchor !== anchors[i])) {
    state.settings.attentionAnchors = reordered;
    scheduleAnchorSave();
  }
}

function closeAnchorManager() {
  state.anchorManagerOpen = false;
  els.anchorManager.classList.remove('open');
  els.anchorManager.setAttribute('aria-hidden', 'true');
}

function updateAnchorManagerCount() {
  const activeCount = configuredActiveAnchors().length;
  const total = state.settings.attentionAnchors.length;
  els.anchorManagerCount.textContent = `${activeCount}/${MAX_ACTIVE_ANCHORS} active · ${total} total`;
  els.btnClearActiveAnchors.disabled = activeCount === 0;
}

function renderAnchorManager() {
  updateAnchorManagerCount();
  const rows = state.settings.attentionAnchors.map((anchor, index) => createAnchorEditor(anchor, index));
  els.anchorManagerList.replaceChildren(...rows);
}

function createAnchorEditor(anchor, index) {
  const row = document.createElement('div');
  row.className = 'anchor-editor';
  row.tabIndex = 0;
  row.dataset.anchorId = anchor.id;
  row.classList.toggle('invalid', !validAnchor(anchor));
  row.classList.toggle('focused', index === state.focusedAnchorIndex);
  row.addEventListener('pointerdown', event => {
    if (event.target.tagName !== 'INPUT') {
      row.focus();
    }
    setFocusedAnchorIndex(index);
  });
  row.addEventListener('focus', () => setFocusedAnchorIndex(index));

  const handle = document.createElement('div');
  handle.className = 'anchor-drag-handle';
  handle.textContent = '⠿';
  handle.title = 'Drag to reorder';
  handle.addEventListener('pointerdown', event => startAnchorDrag(event, row));

  const active = document.createElement('input');
  active.type = 'checkbox';
  active.checked = anchor.active && validAnchor(anchor);
  active.title = 'Active';
  active.addEventListener('change', async () => {
    if (active.checked && !validAnchor(anchor)) {
      active.checked = false;
      showToast('Add content first');
      return;
    }
    if (active.checked && configuredActiveAnchors().length >= MAX_ACTIVE_ANCHORS) {
      active.checked = false;
      showToast(`Up to ${MAX_ACTIVE_ANCHORS} anchors can be active`);
      return;
    }
    await updateAnchor(index, { active: active.checked }, true);
  });

  const shapePreview = document.createElement('div');
  shapePreview.className = `anchor-shape-preview anchor-shape-${index % ANCHOR_SHAPE_VARIANTS} no-image`;
  shapePreview.title = 'Current abstract shape';
  applyShapePattern(shapePreview, anchor.shapePattern);
  shapePreview.addEventListener('click', () => generateAnchorShape(index));

  const imagePreview = document.createElement('button');
  imagePreview.className = 'anchor-image-preview';
  imagePreview.classList.toggle('empty', !anchor.imagePath);
  imagePreview.title = anchor.imagePath ? 'Current image' : 'No image';
  imagePreview.type = 'button';
  imagePreview.addEventListener('click', () => pickAnchorImage(index));

  if (anchor.imagePath) {
    const previewImage = document.createElement('img');
    previewImage.draggable = false;
    previewImage.src = window.clipboardAPI.assetUrl(anchor.imagePath);
    imagePreview.append(previewImage);
  }

  const emoji = document.createElement('input');
  emoji.type = 'text';
  emoji.className = 'emoji-input';
  emoji.maxLength = 8;
  emoji.value = anchor.emoji || '';
  emoji.title = 'Emoji';
  emoji.addEventListener('input', () => {
    state.settings.attentionAnchors[index].emoji = emoji.value;
    refreshEditor();
    updateAnchorsAfterLocalEdit();
  });
  emoji.addEventListener('change', () => saveSettings().catch(() => {}));

  const title = document.createElement('input');
  title.type = 'text';
  title.value = anchor.title || '';
  title.placeholder = 'Title';
  title.title = 'Title';
  title.addEventListener('input', () => {
    state.settings.attentionAnchors[index].title = title.value;
    refreshEditor();
    updateAnchorsAfterLocalEdit();
  });
  title.addEventListener('change', () => saveSettings().catch(() => {}));

  const imageState = document.createElement('span');
  imageState.className = 'anchor-image-state';
  imageState.textContent = anchor.imagePath ? 'Image set' : 'No image';
  imageState.title = anchor.imagePath || 'No image';

  const pick = document.createElement('button');
  pick.textContent = 'Pick';
  pick.title = 'Choose image';
  pick.addEventListener('click', () => pickAnchorImage(index));

  const paste = document.createElement('button');
  paste.textContent = 'Paste';
  paste.title = 'Paste image from clipboard';
  paste.addEventListener('click', () => pasteAnchorImage(index));

  const shape = document.createElement('button');
  shape.textContent = 'Shape';
  shape.title = 'Generate abstract shape pattern';
  shape.addEventListener('click', () => generateAnchorShape(index));

  const remove = document.createElement('button');
  remove.className = 'anchor-remove-image';
  remove.textContent = '×';
  remove.title = 'Remove image';
  remove.disabled = !anchor.imagePath;
  remove.addEventListener('click', () => updateAnchor(index, { imagePath: null }, true));

  const deleteAnchorBtn = document.createElement('button');
  deleteAnchorBtn.className = 'anchor-delete';
  deleteAnchorBtn.textContent = '🗑';
  deleteAnchorBtn.title = 'Delete anchor';
  deleteAnchorBtn.addEventListener('click', () => deleteAnchor(index));

  row.append(handle, active, shapePreview, imagePreview, emoji, title, imageState, pick, paste, shape, remove, deleteAnchorBtn);
  return row;

  function refreshEditor() {
    const current = state.settings.attentionAnchors[index];
    const isValid = validAnchor(current);
    row.classList.toggle('invalid', !isValid);
    active.checked = current.active && isValid;
    updateAnchorManagerCount();
  }
}

function randomInt(min, max) {
  return Math.floor(min + Math.random() * (max - min + 1));
}

function hsl(hue, saturation, lightness, alpha = 1) {
  return `hsl(${Math.round(hue)} ${Math.round(saturation)}% ${Math.round(lightness)}% / ${alpha})`;
}

function generateShapePattern() {
  const hue = randomInt(0, 359);
  const accentA = (hue + randomInt(24, 52)) % 360;
  const accentB = (hue + randomInt(126, 168)) % 360;
  const accentC = (hue + randomInt(198, 238)) % 360;
  return {
    bg: hsl(hue, randomInt(28, 42), randomInt(11, 18)),
    a: hsl(accentA, randomInt(58, 76), randomInt(56, 68), 0.48),
    b: hsl(accentB, randomInt(48, 68), randomInt(48, 62), 0.42),
    c: hsl(accentC, randomInt(34, 58), randomInt(70, 82), 0.16),
    p1: `${randomInt(18, 34)}% ${randomInt(18, 38)}%`,
    p2: `${randomInt(62, 82)}% ${randomInt(58, 78)}%`,
    p3: `${randomInt(42, 68)}% ${randomInt(32, 58)}%`,
    s1: `${randomInt(12, 20)}%`,
    s2: `${randomInt(17, 26)}%`,
    angle: `${randomInt(18, 72)}deg`,
  };
}

async function generateAnchorShape(index) {
  await updateAnchor(index, { shapePattern: generateShapePattern() }, true);
  showToast('Shape generated');
}

function setFocusedAnchorIndex(index) {
  const lastIndex = Math.max(0, state.settings.attentionAnchors.length - 1);
  state.focusedAnchorIndex = Math.max(0, Math.min(lastIndex, index));
  [...els.anchorManagerList.children].forEach((row, rowIndex) => {
    row.classList.toggle('focused', rowIndex === state.focusedAnchorIndex);
  });
}

function updateAnchorsAfterLocalEdit() {
  renderAnchors();
  renderHistory();
  applySettingsClasses();
  scheduleAnchorSave();
}

function scheduleAnchorSave() {
  clearTimeout(state.anchorSaveTimer);
  state.anchorSaveTimer = setTimeout(() => {
    saveSettings().catch(() => {});
  }, 350);
}

async function updateAnchor(index, patch, rerenderManager = false) {
  const current = state.settings.attentionAnchors[index];
  state.settings.attentionAnchors[index] = { ...current, ...patch };
  if (state.selectedAnchorId === current.id && !validAnchor(state.settings.attentionAnchors[index])) {
    state.selectedAnchorId = null;
  }
  renderAnchors();
  renderHistory();
  await saveSettings();
  if (rerenderManager && state.anchorManagerOpen) renderAnchorManager();
}

async function pickAnchorImage(index) {
  let filePath = null;
  try {
    filePath = await window.clipboardAPI.pickImage();
  } catch (error) {
    console.error(error);
    showToast('Could not open file picker');
    return;
  }
  if (!filePath) return;

  try {
    const imagePath = await window.clipboardAPI.addDefaultImage(filePath);
    await updateAnchor(index, { imagePath }, true);
    showToast('Image set');
  } catch (error) {
    console.error(error);
    showToast('Could not add image');
  }
}

async function pasteAnchorImage(index) {
  let imagePath = null;
  try {
    imagePath = await window.clipboardAPI.pasteDefaultImage();
  } catch (error) {
    console.error(error);
    showToast('Could not paste image');
    return;
  }
  if (!imagePath) {
    showToast('No image on clipboard');
    return;
  }
  await updateAnchor(index, { imagePath }, true);
  showToast('Image set');
}

async function addAnchor() {
  state.settings.attentionAnchors.push(blankAnchor());
  state.focusedAnchorIndex = state.settings.attentionAnchors.length - 1;
  renderAnchors();
  renderHistory();
  await saveSettings();
  renderAnchorManager();
  const rows = els.anchorManagerList.children;
  const newRow = rows[rows.length - 1];
  if (newRow) {
    newRow.scrollIntoView({ block: 'nearest' });
    const titleInput = newRow.querySelector('input[type="text"]:not(.emoji-input)');
    if (titleInput) titleInput.focus();
  }
}

// Pointer-based drag-and-drop reordering for manager rows. The dragged row is moved
// among its siblings as the pointer crosses each row's midpoint; holding near the top
// or bottom edge auto-scrolls the list when there are enough rows to scroll.
function startAnchorDrag(event, row) {
  if (event.button != null && event.button !== 0) return;
  event.preventDefault();
  const list = els.anchorManagerList;
  const EDGE = 36;
  const SCROLL_SPEED = 9;
  let pointerY = event.clientY;
  let scrollDir = 0;
  let rafId = null;

  row.classList.add('dragging');
  document.body.classList.add('anchor-dragging');

  function repositionRow() {
    const siblings = [...list.querySelectorAll('.anchor-editor')].filter(item => item !== row);
    const before = siblings.find(item => {
      const rect = item.getBoundingClientRect();
      return pointerY < rect.top + rect.height / 2;
    });
    if (before) {
      if (before.previousElementSibling !== row) list.insertBefore(row, before);
    } else if (list.lastElementChild !== row) {
      list.append(row);
    }
  }

  function tick() {
    if (scrollDir !== 0 && list.scrollHeight > list.clientHeight) {
      list.scrollTop += scrollDir * SCROLL_SPEED;
      repositionRow();
    }
    rafId = requestAnimationFrame(tick);
  }

  function onMove(moveEvent) {
    pointerY = moveEvent.clientY;
    const rect = list.getBoundingClientRect();
    if (pointerY < rect.top + EDGE) scrollDir = -1;
    else if (pointerY > rect.bottom - EDGE) scrollDir = 1;
    else scrollDir = 0;
    repositionRow();
  }

  function onUp() {
    document.removeEventListener('pointermove', onMove);
    document.removeEventListener('pointerup', onUp);
    cancelAnimationFrame(rafId);
    row.classList.remove('dragging');
    document.body.classList.remove('anchor-dragging');
    commitAnchorOrderFromDom(row.dataset.anchorId);
  }

  document.addEventListener('pointermove', onMove);
  document.addEventListener('pointerup', onUp);
  rafId = requestAnimationFrame(tick);
}

async function commitAnchorOrderFromDom(focusAnchorId = null) {
  const ids = [...els.anchorManagerList.querySelectorAll('.anchor-editor')].map(item => item.dataset.anchorId);
  const byId = new Map(state.settings.attentionAnchors.map(anchor => [anchor.id, anchor]));
  const reordered = ids.map(id => byId.get(id)).filter(Boolean);
  if (reordered.length !== state.settings.attentionAnchors.length) {
    // DOM and state disagree (shouldn't happen) — just re-render from state.
    renderAnchorManager();
    return;
  }
  const changed = reordered.some((anchor, i) => anchor !== state.settings.attentionAnchors[i]);
  state.settings.attentionAnchors = reordered;
  if (focusAnchorId) {
    const i = reordered.findIndex(anchor => anchor.id === focusAnchorId);
    if (i >= 0) state.focusedAnchorIndex = i;
  }
  if (changed) {
    renderAnchors();
    renderHistory();
    await saveSettings();
  }
  renderAnchorManager();
}

async function deleteAnchor(index) {
  const list = state.settings.attentionAnchors;
  const removed = list[index];
  if (!removed) return;
  list.splice(index, 1);
  // Keep at least one row so the manager is never empty and an anchor can always be added.
  if (list.length === 0) list.push(blankAnchor());
  if (state.selectedAnchorId === removed.id) state.selectedAnchorId = null;
  state.focusedAnchorIndex = Math.max(0, Math.min(index, list.length - 1));
  renderAnchors();
  renderHistory();
  await saveSettings();
  renderAnchorManager();
}

async function clearActiveAnchors() {
  state.settings.attentionAnchors = state.settings.attentionAnchors.map(anchor => ({
    ...anchor,
    active: false,
  }));
  state.selectedAnchorId = null;
  renderAnchors();
  renderHistory();
  await saveSettings();
  renderAnchorManager();
}

async function toggleDualDisplayers() {
  state.settings.dualDisplayers = !state.settings.dualDisplayers;
  if (!state.settings.dualDisplayers) state.settings.activeDisplayer = 0;
  applySettingsClasses();
  renderDisplayers();
  await saveSettings();
}

async function drainClipboardQueue() {
  if (state.drainInFlight) return;
  state.drainInFlight = true;
  try {
    const items = await window.clipboardAPI.drainClipboardItems();
    for (const item of items) addHistoryItem(item);
  } catch {
    // Clipboard contention is normal on Windows; the next tick gets another chance.
  } finally {
    state.drainInFlight = false;
  }
}

function scheduleClipboardFallbackDrain() {
  clearTimeout(state.pollTimer);
  state.pollTimer = setTimeout(async () => {
    await drainClipboardQueue();
    scheduleClipboardFallbackDrain();
  }, CLIPBOARD_FALLBACK_POLL_MS);
}

async function startClipboardDrain() {
  await drainClipboardQueue();
  try {
    state.clipboardUnlisten = await window.clipboardAPI.onClipboardItemsReady(drainClipboardQueue);
  } catch {
    // The slow fallback keeps the app functional if event wiring is unavailable.
  }
  scheduleClipboardFallbackDrain();
}

function setSettingsOpen(open) {
  state.settingsOpen = open;
  applySettingsClasses();
}

function bindChrome() {
  document.addEventListener('contextmenu', event => {
    event.preventDefault();
  });
  els.titleDrag.addEventListener('mousedown', event => {
    if (event.button === 0) saveWindowSoonAfterDrag();
  });
  els.hiddenDragStrip.addEventListener('mousedown', event => {
    if (event.button === 0) saveWindowSoonAfterDrag();
  });
  els.btnDual.addEventListener('click', toggleDualDisplayers);
  els.btnMirror.addEventListener('click', async () => {
    state.settings.mirrorUi = !state.settings.mirrorUi;
    applySettingsClasses();
    await saveSettings();
  });
  els.btnTopbar.addEventListener('click', toggleTopbar);
  els.btnSettings.addEventListener('click', event => {
    event.stopPropagation();
    setSettingsOpen(!state.settingsOpen);
  });
  els.btnSettingsClose.addEventListener('click', event => {
    event.stopPropagation();
    setSettingsOpen(false);
  });
  els.settingsPanel.addEventListener('click', event => event.stopPropagation());
  els.btnAddDefaultImage.addEventListener('click', addToPool);
  els.btnPasteDefaultImage.addEventListener('click', pasteToPool);
  els.btnImagePickerClose.addEventListener('click', closeImagePicker);
  els.imagePicker.addEventListener('click', event => {
    if (event.target === els.imagePicker) closeImagePicker();
  });
  els.btnAttentionAnchors.addEventListener('click', event => {
    event.stopPropagation();
    openAnchorManager();
  });
  els.btnAddAnchor.addEventListener('click', addAnchor);
  els.btnClearActiveAnchors.addEventListener('click', clearActiveAnchors);
  els.btnAnchorManagerClose.addEventListener('click', closeAnchorManager);
  els.anchorManager.addEventListener('click', event => {
    if (event.target === els.anchorManager) closeAnchorManager();
  });
  els.btnClearAnchors.addEventListener('click', event => {
    event.stopPropagation();
    clearActiveAnchors();
  });
  els.anchorColumn.addEventListener('dblclick', event => {
    if (event.target.closest('#btn-clear-anchors')) return;
    event.preventDefault();
    event.stopPropagation();
    const tile = event.target.closest('.anchor-item');
    openAnchorManager(tile?.dataset.anchorId || null);
  });
  els.settingDualDisplayers.addEventListener('change', async () => {
    state.settings.dualDisplayers = els.settingDualDisplayers.checked;
    if (!state.settings.dualDisplayers) state.settings.activeDisplayer = 0;
    applySettingsClasses();
    renderDisplayers();
    await saveSettings();
  });
  els.settingMirrorUi.addEventListener('change', async () => {
    state.settings.mirrorUi = els.settingMirrorUi.checked;
    applySettingsClasses();
    await saveSettings();
  });
  els.settingHideTopbarStartup.addEventListener('change', async () => {
    state.settings.hideTopbarOnStartup = els.settingHideTopbarStartup.checked;
    await saveSettings();
    showToast(state.settings.hideTopbarOnStartup ? 'Top bar will hide on startup' : 'Top bar will show on startup');
  });
  els.settingRememberWindowPosition.addEventListener('change', async () => {
    state.settings.rememberWindowPosition = els.settingRememberWindowPosition.checked;
    await saveSettings();
    if (state.settings.rememberWindowPosition) {
      await window.clipboardAPI.saveWindowState().catch(() => {});
    }
    showToast(state.settings.rememberWindowPosition ? 'Window position will restore on startup' : 'Window position restore disabled');
  });
  els.settingExpandBorderlessEdges.addEventListener('change', async () => {
    state.settings.expandBorderlessEdges = els.settingExpandBorderlessEdges.checked;
    applySettingsClasses();
    await saveSettings();
    await window.clipboardAPI.adjustWindowBorderlessEdges(state.settings.expandBorderlessEdges).catch(() => {});
    await window.clipboardAPI.saveWindowState().catch(() => {});
    showToast(state.settings.expandBorderlessEdges ? 'Borderless edges expanded' : 'Borderless edges restored');
  });
  els.settingAttentionAnchors.addEventListener('change', async () => {
    const enabled = els.settingAttentionAnchors.checked;
    if (!enabled) {
      await restoreHiddenHistory();
    }
    state.settings.attentionAnchorsEnabled = enabled;
    state.selectedAnchorId = null;
    renderAnchors();
    renderHistory();
    applySettingsClasses();
    await saveSettings();
  });
  els.btnMinimize.addEventListener('click', () => window.clipboardAPI.minimize());
  els.btnClose.addEventListener('click', async () => {
    await window.clipboardAPI.saveWindowState().catch(() => {});
    window.clipboardAPI.close();
  });
  els.btnClear.addEventListener('click', () => {
    state.history = [];
    state.selectedId = null;
    state.selectedItemSnapshot = null;
    renderHistory();
    // Also drop any queued items on the backend and purge the cached image files,
    // otherwise drained items repopulate and the cache grows unbounded for the session.
    window.clipboardAPI.clearHistory().catch(() => {});
  });

  document.addEventListener('click', () => {
    els.contextMenu.classList.remove('open');
    setSettingsOpen(false);
  });
  window.addEventListener('resize', scheduleWindowSave);
  window.addEventListener('blur', scheduleWindowSave);
  window.addEventListener('beforeunload', () => {
    clearTimeout(state.anchorSaveTimer);
    clearTimeout(state.pollTimer);
    if (typeof state.clipboardUnlisten === 'function') state.clipboardUnlisten();
    window.clipboardAPI.saveWindowState().catch(() => {});
    window.clipboardAPI.saveSettings(state.settings).catch(() => {});
  });
}

async function toggleTopbar() {
  state.settings.topbarVisible = !state.settings.topbarVisible;
  applySettingsClasses();
  await saveSettings();
}

function bindKeyboard() {
  document.addEventListener('keydown', async event => {
    if (state.anchorManagerOpen) {
      if (event.key === 'Escape') {
        event.preventDefault();
        closeAnchorManager();
      } else if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'v' && !isTextInput(event.target)) {
        event.preventDefault();
        await pasteAnchorImage(state.focusedAnchorIndex);
      }
      return;
    }

    // While the image picker overlay is open, only Escape (to close it) is handled.
    if (state.imagePickerOpen) {
      if (event.key === 'Escape') {
        event.preventDefault();
        closeImagePicker();
      } else if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'v') {
        event.preventDefault();
        pasteToPool();
      }
      return;
    }

    const key = event.key.toLowerCase();
    if (event.shiftKey && key === 'q' && !event.ctrlKey && !event.altKey && !event.metaKey) {
      event.preventDefault();
      await toggleTopbar();
      return;
    }

    if (event.ctrlKey && key === 'c') {
      event.preventDefault();
      const item = document.activeElement === els.historyPanel ? selectedItem() : activeDisplayer().item || selectedItem();
      if (item && !item.bundled) await copyAndRaise(item);
      return;
    }

    switch (event.key) {
      case 'ArrowUp':
        event.preventDefault();
        moveSelection(-1);
        break;
      case 'ArrowDown':
        event.preventDefault();
        moveSelection(1);
        break;
      case 'Delete':
        event.preventDefault();
        deleteSelected();
        break;
      case 'Escape':
        event.preventDefault();
        if (state.settingsOpen) {
          setSettingsOpen(false);
          break;
        }
        deselectAll();
        break;
      case 'c':
      case 'C':
        if (!event.ctrlKey && !event.metaKey) {
          event.preventDefault();
          resetTransform(activeDisplayer());
        }
        break;
    }
  });
}

function isTextInput(target) {
  return target instanceof HTMLInputElement && target.type === 'text';
}

async function init() {
  state.settings = normalizeSettings(await window.clipboardAPI.loadSettings().catch(defaultSettings));
  state.settings.topbarVisible = !state.settings.hideTopbarOnStartup;
  applySettingsClasses();
  buildDisplayers();
  bindChrome();
  bindKeyboard();
  setActiveDisplayer(state.settings.activeDisplayer);
  renderAnchors();
  renderHistory();
  // Window position is restored natively at startup (see src-tauri setup), so there is no
  // JS restore here — doing both caused a visible jump on launch.
  if (state.settings.expandBorderlessEdges && (!state.settings.rememberWindowPosition || !state.settings.window)) {
    await window.clipboardAPI.adjustWindowBorderlessEdges(true).catch(() => {});
    await window.clipboardAPI.saveWindowState().catch(() => {});
  }
  startClipboardDrain();
}

async function start() {
  // The Tauri bridge is normally injected before this runs, but at early boot it can lag a
  // few ticks. Wait briefly so a transient timing gap never kills the app outright.
  for (let attempt = 0; attempt < 20 && !window.clipboardAPI.isReady(); attempt += 1) {
    await new Promise(resolve => setTimeout(resolve, 50));
  }
  try {
    await init();
  } catch (error) {
    console.error('Initialization failed', error);
  } finally {
    // The window starts hidden; always reveal it so a failure never leaves a black frame.
    window.clipboardAPI.show().catch(() => {});
  }
}

start();
