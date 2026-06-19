'use strict';

const img = document.getElementById('image');
const closeZone = document.getElementById('close-zone');
const btnClose = document.getElementById('btn-close');
const btnDrag = document.getElementById('btn-drag');
const contextMenu = document.getElementById('context-menu');
const menuCopy = document.getElementById('menu-copy');
const menuClose = document.getElementById('menu-close');

let currentPath = null;
let keepVisiblePinned = false;

// --- Zoom & pan state ---------------------------------------------------
// The <img> fills the window; we transform it (origin 0,0) to zoom/pan.
const MIN_SCALE = 0.1;
const MAX_SCALE = 20;
let scale = 1;
let panX = 0;
let panY = 0;

function applyTransform(animate = false) {
  if (animate) {
    img.style.transition = 'transform 0.3s ease-out';
    img.addEventListener('transitionend', () => {
      img.style.transition = '';
    }, { once: true });
  } else {
    img.style.transition = '';
  }
  img.style.transform = `translate(${panX}px, ${panY}px) scale(${scale})`;
}

function recenter() {
  scale = 1;
  panX = 0;
  panY = 0;
  applyTransform(true);
}

function zoomAt(clientX, clientY, factor) {
  const next = Math.min(MAX_SCALE, Math.max(MIN_SCALE, scale * factor));
  if (next === scale) return;
  // Keep the point under the cursor fixed: screen = pan + scale * boxPoint.
  panX = clientX - (clientX - panX) * (next / scale);
  panY = clientY - (clientY - panY) * (next / scale);
  scale = next;
  applyTransform();
}

const isZoomed = () => scale !== 1 || panX !== 0 || panY !== 0;

function showError(message) {
  document.body.classList.add('load-error');
  document.body.dataset.error = message;
  img.removeAttribute('src');
  img.alt = message;
}

async function init() {
  // The Tauri bridge is normally injected before this runs; wait briefly just in case.
  for (let attempt = 0; attempt < 20 && !window.clipboardAPI.isReady(); attempt += 1) {
    await new Promise(resolve => setTimeout(resolve, 50));
  }
  try {
    currentPath = await window.clipboardAPI.getAssignedImagePath();
    if (!currentPath) {
      showError('Image path was not assigned.');
      return;
    }
    img.src = window.clipboardAPI.assetUrl(currentPath);
  } catch (error) {
    console.error('Failed to load image:', error);
    showError('Failed to load image.');
  }
}

function closeWindow() {
  window.clipboardAPI.close().catch(() => window.close());
}

function copyImage() {
  if (!currentPath) return;
  window.clipboardAPI.copyItem({ kind: 'image', filePath: currentPath }).catch(error => console.error(error));
}

function openContextMenu(x, y) {
  contextMenu.classList.add('open');
  const maxX = window.innerWidth - contextMenu.offsetWidth - 4;
  const maxY = window.innerHeight - contextMenu.offsetHeight - 4;
  contextMenu.style.left = `${Math.max(4, Math.min(x, maxX))}px`;
  contextMenu.style.top = `${Math.max(4, Math.min(y, maxY))}px`;
}

function closeContextMenu() {
  contextMenu.classList.remove('open');
}

function isInteractiveTarget(target) {
  return closeZone.contains(target) || contextMenu.contains(target);
}

// Outer band (≈ a window title bar's thickness) drags the OS window; the
// inner area pans the zoomed image.
const EDGE_THICKNESS = 32;

function isEdgeZone(clientX, clientY) {
  return clientX <= EDGE_THICKNESS ||
    clientY <= EDGE_THICKNESS ||
    clientX >= window.innerWidth - EDGE_THICKNESS ||
    clientY >= window.innerHeight - EDGE_THICKNESS;
}

let panPointerId = null;
let panStartX = 0;
let panStartY = 0;
let panOriginX = 0;
let panOriginY = 0;

function startWindowDrag(e) {
  if (e.button !== 0 || e.detail > 1 || isInteractiveTarget(e.target)) return;
  closeContextMenu();
  // The edge band always moves the window, regardless of zoom.
  if (isEdgeZone(e.clientX, e.clientY)) {
    window.clipboardAPI.startWindowDrag().catch(error => console.error(error));
    return;
  }
  // The center pans the image when zoomed in.
  if (isZoomed()) {
    panPointerId = e.pointerId;
    panStartX = e.clientX;
    panStartY = e.clientY;
    panOriginX = panX;
    panOriginY = panY;
    img.setPointerCapture(panPointerId);
    img.style.cursor = 'grabbing';
    e.preventDefault();
  }
}

img.addEventListener('pointermove', (e) => {
  if (panPointerId === null || e.pointerId !== panPointerId) return;
  panX = panOriginX + (e.clientX - panStartX);
  panY = panOriginY + (e.clientY - panStartY);
  applyTransform();
});

function endPan(e) {
  if (panPointerId === null || e.pointerId !== panPointerId) return;
  try { img.releasePointerCapture(panPointerId); } catch { /* ignore */ }
  panPointerId = null;
  img.style.cursor = '';
}

// Cursor hint: move over the edge band, grab over the pannable center.
document.addEventListener('pointermove', (e) => {
  if (panPointerId !== null) return;
  if (isInteractiveTarget(e.target)) {
    img.style.cursor = '';
  } else if (isEdgeZone(e.clientX, e.clientY)) {
    img.style.cursor = 'move';
  } else {
    img.style.cursor = isZoomed() ? 'grab' : '';
  }
});
img.addEventListener('pointerup', endPan);
img.addEventListener('pointercancel', endPan);

img.addEventListener('wheel', (e) => {
  e.preventDefault();
  zoomAt(e.clientX, e.clientY, e.deltaY < 0 ? 1.1 : 1 / 1.1);
}, { passive: false });

btnClose.addEventListener('click', closeWindow);
btnDrag.addEventListener('pointerdown', (e) => {
  if (e.button !== 0) return;
  closeContextMenu();
  e.preventDefault();
  // Keep the controls pinned during the native drag (which can drop :hover
  // and fire a spurious pointerleave) and afterwards until the pointer
  // actually moves away from the zone.
  keepVisiblePinned = true;
  closeZone.classList.add('keep-visible');
  window.clipboardAPI.startWindowDrag().catch(error => console.error(error));
});

// Un-pin only once the cursor has genuinely left the zone's vicinity.
document.addEventListener('mousemove', (e) => {
  if (!keepVisiblePinned) return;
  // Ignore events fired while a button is held — i.e. during the native
  // drag loop, where the cursor can report stray coordinates.
  if (e.buttons !== 0) return;
  const r = closeZone.getBoundingClientRect();
  const margin = 12;
  const outside =
    e.clientX < r.left - margin || e.clientX > r.right + margin ||
    e.clientY < r.top - margin || e.clientY > r.bottom + margin;
  if (outside) {
    keepVisiblePinned = false;
    closeZone.classList.remove('keep-visible');
  }
});
img.addEventListener('dblclick', closeWindow);
document.addEventListener('pointerdown', startWindowDrag);

img.addEventListener('contextmenu', (e) => {
  e.preventDefault();
  openContextMenu(e.clientX, e.clientY);
});

document.addEventListener('click', (e) => {
  if (!contextMenu.contains(e.target)) closeContextMenu();
});

menuCopy.addEventListener('click', () => {
  closeContextMenu();
  copyImage();
});

menuClose.addEventListener('click', () => {
  closeContextMenu();
  closeWindow();
});

document.addEventListener('keydown', (e) => {
  if (e.key === 'Escape') {
    if (contextMenu.classList.contains('open')) {
      closeContextMenu();
    } else {
      closeWindow();
    }
    return;
  }
  if (e.key === 'Delete') {
    e.preventDefault();
    closeWindow();
    return;
  }
  if (e.key === 'c' || e.key === 'C') {
    e.preventDefault();
    if (e.ctrlKey || e.metaKey) {
      copyImage();
    } else {
      recenter();
    }
  }
});

init();
