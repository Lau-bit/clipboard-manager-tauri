use arboard::{Clipboard, ImageData};
use image::{
    codecs::png::{CompressionType, FilterType, PngEncoder},
    imageops, ColorType, ImageEncoder, ImageFormat, ImageReader, RgbaImage,
};
use serde::{Deserialize, Serialize};
use std::{
    borrow::Cow,
    collections::{hash_map::DefaultHasher, HashMap, VecDeque},
    fs::{self, File},
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
        mpsc::{self, Receiver},
        Arc, Mutex,
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tauri::{
    utils::config::Color, AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, Position, Size,
    WebviewUrl, WebviewWindow, WebviewWindowBuilder, WindowEvent,
};

#[cfg(windows)]
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
#[cfg(windows)]
use windows_sys::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DwmSetWindowAttribute};
#[cfg(windows)]
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
#[cfg(windows)]
use windows_sys::Win32::System::DataExchange::{
    AddClipboardFormatListener, CloseClipboard, GetClipboardData, GetClipboardSequenceNumber,
    IsClipboardFormatAvailable, OpenClipboard,
};
#[cfg(windows)]
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
#[cfg(windows)]
use windows_sys::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
#[cfg(windows)]
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, GetWindowLongPtrW,
    GetWindowRect, IsIconic, IsZoomed, KillTimer, PostQuitMessage, RegisterClassW, SetTimer,
    SetWindowLongPtrW, TranslateMessage, GWLP_USERDATA, HWND_MESSAGE, MSG, WM_CLIPBOARDUPDATE,
    WM_DESTROY, WM_TIMER, WNDCLASSW,
};

const HISTORY_CACHE_DIR: &str = "clipboard-history";
const DEFAULTS_DIR: &str = "default-images";
const SETTINGS_FILE: &str = "settings.json";
const HIDDEN_HISTORY_FILE: &str = "attention-anchor-hidden-history.json";
const IMAGE_EXTS: &[&str] = &["jpg", "jpeg", "png", "gif", "bmp", "webp", "ico"];
// The clipboard history grid always has a fixed 6 rows; the frontend's column setting (1-5)
// is what actually determines how many items are visible at once (columns * rows).
const HISTORY_ROWS: usize = 6;
const HISTORY_COLUMNS_MIN: u32 = 1;
const HISTORY_COLUMNS_MAX: u32 = 5;
const HISTORY_COLUMNS_DEFAULT: u32 = 3;
// Max items ever retained/sent to the frontend, across every possible column choice, so
// raising the column count later doesn't need to wait for fresh clipboard captures.
const HISTORY_LIMIT: usize = HISTORY_COLUMNS_MAX as usize * HISTORY_ROWS;
const HISTORY_THUMB_MAX_EDGE: u32 = 360;
// Largest "fill bias" nudge accepted for the history-thumbnail fill mode. The amount is in
// source-image pixels (each step shifts the cover crop 1px of the image); the per-thumbnail
// renderer clamps it to whatever crop room each image actually has, so this is just a sane cap.
const THUMBNAIL_FILL_BIAS_MAX: u32 = 4000;
// Newest clipboard cache files to keep on disk. A fresh PNG (and sometimes a thumbnail) is written
// for every clipboard change, but only the most recent ~30 items are ever shown. Without trimming,
// the cache grows without bound across a long-running session (days of sleep/wake with no restart),
// and every stale file stays a distinct asset URL the WebView2 image cache holds a decoded bitmap
// for — which is what eventually drives the webview out of memory. The startup wipe only helps if
// the app is actually restarted, so trim continuously as items arrive. The limit comfortably covers
// the visible (<=30) plus hidden (6) history, each using at most an image plus a thumbnail.
const HISTORY_CACHE_FILE_LIMIT: usize = 96;
const BORDERLESS_EDGE_EXPAND: i32 = 1;
// Native window minimum width. With the displayers column showing, it needs room for that
// column's own 200px minimum plus a usable history grid; with displayers disabled, only the
// history grid needs to fit, so the window can narrow much further.
const MIN_WINDOW_WIDTH_WITH_DISPLAYERS: f64 = 520.0;
const MIN_WINDOW_WIDTH_WITHOUT_DISPLAYERS: f64 = 260.0;
const MIN_WINDOW_HEIGHT: f64 = 360.0;
const CLIPBOARD_COPY_ATTEMPTS: usize = 6;
const CLIPBOARD_COPY_RETRY_DELAY: Duration = Duration::from_millis(35);
// A clipboard-changed notification can arrive while the app that made the change still holds
// the clipboard open, so a read can briefly fail. Retry a few times before giving up on a
// revision (there is no periodic re-poll to fall back on anymore).
const CLIPBOARD_READ_ATTEMPTS: usize = 6;
const CLIPBOARD_READ_RETRY_DELAY: Duration = Duration::from_millis(20);
// If every read attempt found the clipboard locked, the revision is NOT abandoned — a timer
// on the listener window brings us back to it. Without that, a revision that happened to be
// unreadable for ~120ms would be marked consumed and the clipboard's current contents would
// never reach the history at all, since nothing else ever re-reads the clipboard.
#[cfg(windows)]
const CLIPBOARD_RECHECK_TIMER_ID: usize = 1;
#[cfg(windows)]
const CLIPBOARD_RECHECK_DELAY_MS: u32 = 200;
#[cfg(windows)]
const CLIPBOARD_RECHECK_MAX_ROUNDS: u32 = 25;
// Attempts to open the clipboard for the native bitmap fallback (5ms apart).
#[cfg(windows)]
const CLIPBOARD_OPEN_ATTEMPTS: usize = 8;
#[cfg(windows)]
const CF_DIB: u32 = 8;
#[cfg(windows)]
const CF_DIBV5: u32 = 17;
#[cfg(windows)]
const BI_RGB: u32 = 0;
#[cfg(windows)]
const BI_BITFIELDS: u32 = 3;
// Sanity ceiling for a DIB we will decode ourselves, so a corrupt header cannot make us
// allocate an absurd buffer.
#[cfg(windows)]
const DIB_MAX_EDGE: u32 = 32768;

#[cfg(windows)]
const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
#[cfg(windows)]
const DWMWCP_DONOTROUND: u32 = 1;
#[cfg(windows)]
const DWMWCP_ROUND: u32 = 2;
// The window's visible frame, excluding the invisible resize border WS_THICKFRAME adds
// (~7px on this app's windows) that GetWindowRect counts as part of the window.
#[cfg(windows)]
const DWMWA_EXTENDED_FRAME_BOUNDS: u32 = 9;
// Physical-pixel slack for calling a window edge "flush" with the monitor work area.
#[cfg(windows)]
const SNAP_EDGE_TOLERANCE: i32 = 4;
// A snapped window always covers at least about half of the work area along one axis: halves
// and thirds span its full height, quarters span half of both. Requiring that alongside flush
// edges keeps a small floating image nudged into a screen corner from reading as snapped.
#[cfg(windows)]
const SNAP_SPAN_PERCENT: i32 = 45;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct DisplayerSettings {
    mode: String,
    default_image_path: Option<String>,
    background: String,
    #[serde(default)]
    default_image_zoom_to_fill: bool,
    #[serde(default)]
    clipboard_image_zoom_to_fill: bool,
}

impl Default for DisplayerSettings {
    fn default() -> Self {
        Self {
            mode: "defaultImage".to_string(),
            default_image_path: None,
            background: "solid".to_string(),
            default_image_zoom_to_fill: false,
            clipboard_image_zoom_to_fill: false,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct AttentionAnchorSettings {
    #[serde(default)]
    id: String,
    #[serde(default = "default_true")]
    active: bool,
    #[serde(default)]
    emoji: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    image_path: Option<String>,
    #[serde(default)]
    shape_pattern: Option<serde_json::Value>,
}

fn default_attention_anchors_enabled() -> bool {
    true
}

fn default_attention_anchors() -> Vec<AttentionAnchorSettings> {
    (1..=6)
        .map(|index| AttentionAnchorSettings {
            id: format!("anchor-{index}"),
            active: false,
            emoji: String::new(),
            title: String::new(),
            image_path: None,
            shape_pattern: None,
        })
        .collect()
}

fn is_legacy_default_anchor(index: usize, anchor: &AttentionAnchorSettings) -> bool {
    const IDS: [&str; 6] = [
        "next-thread",
        "rough-edge",
        "tiny-fix",
        "write-note",
        "unstick-path",
        "capture-reference",
    ];
    const EMOJIS: [&str; 6] = ["🧭", "🔎", "✅", "📝", "🛠️", "🖼️"];
    const TITLES: [&str; 6] = [
        "Open the next code thread",
        "Review one rough edge",
        "Ship a tiny fix",
        "Write the note down",
        "Refactor a stuck path",
        "Capture a useful image",
    ];

    index < IDS.len()
        && anchor.id == IDS[index]
        && anchor.active
        && anchor.emoji == EMOJIS[index]
        && anchor.title == TITLES[index]
        && anchor.image_path.is_none()
        && anchor.shape_pattern.is_none()
}


// Stored in logical (scale-independent) pixels so the window restores to the same visual
// size and place across monitors with different DPI scaling.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct WindowState {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

fn default_true() -> bool {
    true
}

// How large a floating image window opens. `0` is the sentinel "Default" fit mode; any other
// value is a percent of the image's true (native) size, where `100` == "True size". Kept in
// [OPENED_IMAGE_SIZE_MIN_PERCENT, OPENED_IMAGE_SIZE_MAX_PERCENT] when not the sentinel.
const OPENED_IMAGE_SIZE_DEFAULT: i32 = 0;
const OPENED_IMAGE_SIZE_MIN_PERCENT: i32 = 25;
const OPENED_IMAGE_SIZE_MAX_PERCENT: i32 = 100;

fn default_opened_image_size() -> i32 {
    OPENED_IMAGE_SIZE_DEFAULT
}

fn default_history_columns() -> u32 {
    HISTORY_COLUMNS_DEFAULT
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Settings {
    mirror_ui: bool,
    topbar_visible: bool,
    #[serde(default)]
    hide_topbar_on_startup: bool,
    #[serde(default = "default_true")]
    remember_window_position: bool,
    #[serde(default)]
    expand_borderless_edges: bool,
    #[serde(default = "default_attention_anchors_enabled")]
    attention_anchors_enabled: bool,
    #[serde(default = "default_attention_anchors")]
    attention_anchors: Vec<AttentionAnchorSettings>,
    // History thumbnails ("small clipboard previews"): off shows each preview at natural size
    // (never upscaled); on fills the cell by cover-cropping, with an optional directional bias.
    #[serde(default)]
    thumbnail_zoom_to_fill: bool,
    #[serde(default)]
    thumbnail_fill_bias_direction: String,
    #[serde(default)]
    thumbnail_fill_bias_amount: u32,
    // Pan portrait-shaped previews' cover crop toward the top (faces) instead of centering.
    #[serde(default = "default_true")]
    portrait_top_bias: bool,
    // How many columns the clipboard history grid is arranged into (rows are derived from
    // this so the same fixed number of history slots always fits, see `HISTORY_COLUMNS_*`).
    #[serde(default = "default_history_columns")]
    history_columns: u32,
    // Whether the image displayers column shows at all. Off lets the window narrow past the
    // width the displayers column would otherwise require (see `apply_window_min_size`).
    #[serde(default = "default_true")]
    displayers_enabled: bool,
    // How big a clicked-open floating image window is: 0 = "Default" (fit a comfortable share
    // of the monitor, never upscaled past native); 25..=100 = that percent of the image's true
    // size, where 100 is "True size" (native pixels). Always clamped to fit the monitor.
    #[serde(default = "default_opened_image_size")]
    opened_image_size: i32,
    dual_displayers: bool,
    active_displayer: usize,
    max_history: usize,
    window: Option<WindowState>,
    displayers: Vec<DisplayerSettings>,
}

impl Default for Settings {
    fn default() -> Self {
        let mut first = DisplayerSettings::default();
        first.mode = "defaultImage".to_string();

        let mut second = DisplayerSettings::default();
        second.mode = "clipboard".to_string();

        Self {
            mirror_ui: false,
            topbar_visible: true,
            hide_topbar_on_startup: false,
            remember_window_position: true,
            expand_borderless_edges: false,
            attention_anchors_enabled: true,
            attention_anchors: default_attention_anchors(),
            thumbnail_zoom_to_fill: false,
            thumbnail_fill_bias_direction: String::new(),
            thumbnail_fill_bias_amount: 0,
            portrait_top_bias: true,
            history_columns: HISTORY_COLUMNS_DEFAULT,
            displayers_enabled: true,
            opened_image_size: OPENED_IMAGE_SIZE_DEFAULT,
            dual_displayers: false,
            active_displayer: 0,
            max_history: HISTORY_LIMIT,
            window: None,
            displayers: vec![first, second],
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ClipboardItem {
    id: String,
    kind: String,
    text: Option<String>,
    file_path: Option<String>,
    thumbnail_path: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    fingerprint: String,
    created_at: u128,
}

#[derive(Debug)]
enum RawClipboardItem {
    Image {
        sequence: u32,
        created_at: u128,
        width: usize,
        height: usize,
        bytes: Vec<u8>,
    },
    Text {
        sequence: u32,
        created_at: u128,
        text: String,
    },
    FileImage(ClipboardItem),
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WindowBounds {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

struct AppState {
    pending_items: Mutex<VecDeque<ClipboardItem>>,
    ignored_sequences: Mutex<VecDeque<u32>>,
    own_clipboard_write_active: AtomicBool,
    window_shown: AtomicBool,
    image_window_counter: AtomicUsize,
    /// Floating image window label -> source file path.
    image_paths: Mutex<HashMap<String, String>>,
    /// Floating image window label -> the app window that opened it.
    image_owners: Mutex<HashMap<String, String>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            pending_items: Mutex::new(VecDeque::new()),
            ignored_sequences: Mutex::new(VecDeque::new()),
            own_clipboard_write_active: AtomicBool::new(false),
            window_shown: AtomicBool::new(false),
            image_window_counter: AtomicUsize::new(0),
            image_paths: Mutex::new(HashMap::new()),
            image_owners: Mutex::new(HashMap::new()),
        }
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

fn hash_value<T: Hash>(value: &T) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn normalize_settings(mut settings: Settings) -> Settings {
    let defaults = Settings::default();
    while settings.displayers.len() < 2 {
        settings
            .displayers
            .push(defaults.displayers[settings.displayers.len()].clone());
    }
    settings.displayers.truncate(2);
    // Anchors are unlimited; only seed the default set when none exist. Never truncate —
    // the UI allows creating arbitrarily many anchors.
    if settings.attention_anchors.is_empty() {
        settings.attention_anchors = defaults.attention_anchors.clone();
    }

    for displayer in &mut settings.displayers {
        if !matches!(
            displayer.mode.as_str(),
            "clipboard" | "defaultImage" | "sticky"
        ) {
            displayer.mode = "clipboard".to_string();
        }
        if !matches!(displayer.background.as_str(), "solid" | "checker") {
            displayer.background = "solid".to_string();
        }
    }

    for (index, anchor) in settings.attention_anchors.iter_mut().enumerate() {
        if is_legacy_default_anchor(index, anchor) {
            *anchor = defaults.attention_anchors[index].clone();
            continue;
        }
        if anchor.id.trim().is_empty() {
            anchor.id = format!("anchor-{}", index + 1);
        }
        if anchor.emoji.chars().count() > 8 {
            anchor.emoji = anchor.emoji.chars().take(8).collect();
        }
        if anchor.image_path.as_deref() == Some("") {
            anchor.image_path = None;
        }
        if !matches!(anchor.shape_pattern, Some(serde_json::Value::Object(_))) {
            anchor.shape_pattern = None;
        }
    }

    if settings.active_displayer > 1 {
        settings.active_displayer = 0;
    }
    settings.max_history = HISTORY_LIMIT;
    settings.history_columns = settings
        .history_columns
        .clamp(HISTORY_COLUMNS_MIN, HISTORY_COLUMNS_MAX);
    if settings.opened_image_size != OPENED_IMAGE_SIZE_DEFAULT {
        settings.opened_image_size = settings
            .opened_image_size
            .clamp(OPENED_IMAGE_SIZE_MIN_PERCENT, OPENED_IMAGE_SIZE_MAX_PERCENT);
    }

    if !matches!(
        settings.thumbnail_fill_bias_direction.as_str(),
        "L" | "R" | "U" | "D"
    ) {
        settings.thumbnail_fill_bias_direction = String::new();
    }
    settings.thumbnail_fill_bias_amount = settings
        .thumbnail_fill_bias_amount
        .min(THUMBNAIL_FILL_BIAS_MAX);
    if settings.thumbnail_fill_bias_direction.is_empty() {
        settings.thumbnail_fill_bias_amount = 0;
    } else if settings.thumbnail_fill_bias_amount == 0 {
        settings.thumbnail_fill_bias_direction = String::new();
    }

    settings
}

fn settings_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_data_dir()
        .map_err(|error| format!("Failed to resolve app data directory: {error}"))?
        .join(SETTINGS_FILE))
}

fn hidden_history_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_data_dir()
        .map_err(|error| format!("Failed to resolve app data directory: {error}"))?
        .join(HIDDEN_HISTORY_FILE))
}

fn load_settings_inner(app: &AppHandle) -> Settings {
    settings_path(app)
        .ok()
        .and_then(|path| fs::read_to_string(path).ok())
        .and_then(|data| serde_json::from_str::<Settings>(&data).ok())
        .map(normalize_settings)
        .unwrap_or_else(Settings::default)
}

fn save_settings_inner(app: &AppHandle, settings: &Settings) -> Result<(), String> {
    let path = settings_path(app)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("Failed to create settings directory: {error}"))?;
    }
    let data = serde_json::to_string_pretty(&normalize_settings(settings.clone()))
        .map_err(|error| format!("Failed to serialize settings: {error}"))?;
    fs::write(path, data).map_err(|error| format!("Failed to save settings: {error}"))
}

fn cache_dir(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_cache_dir()
        .map_err(|error| format!("Failed to resolve app cache directory: {error}"))?
        .join(HISTORY_CACHE_DIR))
}

fn write_rgba_png(path: &Path, width: u32, height: u32, bytes: &[u8]) -> Result<(), String> {
    let file =
        File::create(path).map_err(|error| format!("Failed to create image file: {error}"))?;
    let encoder = PngEncoder::new_with_quality(file, CompressionType::Fast, FilterType::NoFilter);
    encoder
        .write_image(bytes, width, height, ColorType::Rgba8.into())
        .map_err(|error| format!("Failed to save image: {error}"))
}

/// Largest size that fits within `max_edge` on both sides while preserving aspect ratio.
/// `imageops::thumbnail` resizes to the *exact* dimensions it's given (it does NOT keep aspect),
/// so passing a square target squashes non-square images — always feed it these fitted dims.
fn thumbnail_dimensions(width: u32, height: u32, max_edge: u32) -> (u32, u32) {
    if width <= max_edge && height <= max_edge {
        return (width.max(1), height.max(1));
    }
    let scale = f64::from(max_edge) / f64::from(width.max(height));
    let scaled_width = (f64::from(width) * scale).round() as u32;
    let scaled_height = (f64::from(height) * scale).round() as u32;
    (scaled_width.max(1), scaled_height.max(1))
}

fn thumbnail_path_for(path: &Path) -> PathBuf {
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("clip");
    path.with_file_name(format!("{stem}-thumb.png"))
}

fn save_thumbnail_from_rgba(
    path: &Path,
    width: u32,
    height: u32,
    bytes: &[u8],
) -> Result<Option<String>, String> {
    if width <= HISTORY_THUMB_MAX_EDGE && height <= HISTORY_THUMB_MAX_EDGE {
        return Ok(None);
    }
    let Some(image) = RgbaImage::from_raw(width, height, bytes.to_vec()) else {
        return Ok(None);
    };
    let (thumb_w, thumb_h) = thumbnail_dimensions(width, height, HISTORY_THUMB_MAX_EDGE);
    let thumb = imageops::thumbnail(&image, thumb_w, thumb_h);
    let thumb_path = thumbnail_path_for(path);
    write_rgba_png(&thumb_path, thumb.width(), thumb.height(), thumb.as_raw())?;
    Ok(Some(thumb_path.to_string_lossy().to_string()))
}

fn save_thumbnail_from_file(
    app: &AppHandle,
    source: &Path,
    sequence: u32,
    created_at: u128,
) -> Result<Option<String>, String> {
    let image = ImageReader::open(source)
        .map_err(|error| format!("Failed to open clipboard image file: {error}"))?
        .with_guessed_format()
        .map_err(|error| format!("Failed to detect clipboard image format: {error}"))?
        .decode()
        .map_err(|error| format!("Failed to decode clipboard image: {error}"))?
        .to_rgba8();
    if image.width() <= HISTORY_THUMB_MAX_EDGE && image.height() <= HISTORY_THUMB_MAX_EDGE {
        return Ok(None);
    }
    let dir = cache_dir(app)?;
    fs::create_dir_all(&dir)
        .map_err(|error| format!("Failed to create clipboard image cache: {error}"))?;
    let (thumb_w, thumb_h) = thumbnail_dimensions(image.width(), image.height(), HISTORY_THUMB_MAX_EDGE);
    let thumb = imageops::thumbnail(&image, thumb_w, thumb_h);
    let path = dir.join(format!("clip-file-{sequence}-{created_at}-thumb.png"));
    write_rgba_png(&path, thumb.width(), thumb.height(), thumb.as_raw())?;
    Ok(Some(path.to_string_lossy().to_string()))
}

fn default_images_dir(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_data_dir()
        .map_err(|error| format!("Failed to resolve app data directory: {error}"))?
        .join(DEFAULTS_DIR))
}

fn clean_history_cache(app: &AppHandle) {
    if let Ok(dir) = cache_dir(app) {
        if dir.exists() {
            let _ = fs::remove_dir_all(dir);
        }
    }
}

/// Keep the clipboard cache bounded while the app runs: delete all but the newest
/// `HISTORY_CACHE_FILE_LIMIT` files. Called on every capture so a long-lived session never
/// accumulates an unbounded set of cached images (and the asset URLs the webview caches for them).
fn trim_history_cache(app: &AppHandle) {
    let Ok(dir) = cache_dir(app) else {
        return;
    };
    let Ok(entries) = fs::read_dir(&dir) else {
        return;
    };
    let mut files: Vec<(SystemTime, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_file() {
                return None;
            }
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((modified, path))
        })
        .collect();
    if files.len() <= HISTORY_CACHE_FILE_LIMIT {
        return;
    }
    // Newest first, then drop everything past the keep window.
    files.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, path) in files.into_iter().skip(HISTORY_CACHE_FILE_LIMIT) {
        let _ = fs::remove_file(path);
    }
}

fn is_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| {
            IMAGE_EXTS
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(ext))
        })
        .unwrap_or(false)
}

#[cfg(windows)]
fn clipboard_sequence() -> u32 {
    unsafe { GetClipboardSequenceNumber() }
}

#[cfg(not(windows))]
fn clipboard_sequence() -> u32 {
    0
}

fn image_item_from_rgba_with_timestamp(
    app: &AppHandle,
    sequence: u32,
    created_at: u128,
    width: usize,
    height: usize,
    bytes: Vec<u8>,
) -> Result<ClipboardItem, String> {
    let width_u32 = u32::try_from(width).map_err(|_| "Clipboard image is too wide.".to_string())?;
    let height_u32 =
        u32::try_from(height).map_err(|_| "Clipboard image is too tall.".to_string())?;
    if bytes.len() != width.saturating_mul(height).saturating_mul(4) {
        return Err("Clipboard image data is invalid.".to_string());
    }

    let dir = cache_dir(app)?;
    fs::create_dir_all(&dir)
        .map_err(|error| format!("Failed to create clipboard image cache: {error}"))?;

    let path = dir.join(format!("clip-{sequence}-{created_at}.png"));
    write_rgba_png(&path, width_u32, height_u32, &bytes)?;
    let thumbnail_path = save_thumbnail_from_rgba(&path, width_u32, height_u32, &bytes)
        .ok()
        .flatten();

    let fingerprint = format!("image:{width}x{height}:{}", hash_value(&bytes));
    Ok(ClipboardItem {
        id: format!("{sequence}-{created_at}"),
        kind: "image".to_string(),
        text: None,
        file_path: Some(path.to_string_lossy().to_string()),
        thumbnail_path,
        width: Some(width_u32),
        height: Some(height_u32),
        fingerprint,
        created_at,
    })
}

fn text_item(sequence: u32, created_at: u128, text: String) -> Option<ClipboardItem> {
    if text.is_empty() {
        return None;
    }
    Some(ClipboardItem {
        id: format!("{sequence}-{created_at}"),
        kind: "text".to_string(),
        text: Some(text.clone()),
        file_path: None,
        thumbnail_path: None,
        width: None,
        height: None,
        fingerprint: format!("text:{}:{}", text.len(), hash_value(&text)),
        created_at,
    })
}

fn image_item_from_file(
    app: &AppHandle,
    file_path: PathBuf,
    sequence: u32,
) -> Result<ClipboardItem, String> {
    let reader = ImageReader::open(&file_path)
        .map_err(|error| format!("Failed to open clipboard image file: {error}"))?
        .with_guessed_format()
        .map_err(|error| format!("Failed to detect clipboard image format: {error}"))?;
    let dimensions = reader
        .into_dimensions()
        .map_err(|error| format!("Failed to read clipboard image dimensions: {error}"))?;
    let metadata = fs::metadata(&file_path).ok();
    let modified = metadata
        .and_then(|meta| meta.modified().ok())
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    let created_at = now_ms();
    let thumbnail_path = save_thumbnail_from_file(app, &file_path, sequence, created_at)
        .ok()
        .flatten();
    let fingerprint = format!(
        "image-file:{}:{}:{}x{}",
        file_path.to_string_lossy(),
        modified,
        dimensions.0,
        dimensions.1
    );

    Ok(ClipboardItem {
        id: format!("{sequence}-{created_at}"),
        kind: "image".to_string(),
        text: None,
        file_path: Some(file_path.to_string_lossy().to_string()),
        thumbnail_path,
        width: Some(dimensions.0),
        height: Some(dimensions.1),
        fingerprint,
        created_at,
    })
}

fn raw_file_image_from_paths(
    app: &AppHandle,
    paths: Vec<PathBuf>,
    sequence: u32,
) -> Option<RawClipboardItem> {
    paths
        .into_iter()
        .find(|path| path.is_file() && is_image_path(path))
        .and_then(|path| image_item_from_file(app, path, sequence).ok())
        .map(RawClipboardItem::FileImage)
}

/// What one look at the clipboard found. `Nothing` and `Unreadable` are deliberately
/// distinct: the first means the clipboard genuinely holds no format this app supports and
/// the revision is finished with, the second means someone else had the clipboard open and
/// the revision must be tried again.
enum ClipboardRead {
    Item(Box<RawClipboardItem>),
    Nothing,
    Unreadable,
}

#[cfg(windows)]
struct ClipboardSession;

#[cfg(windows)]
impl ClipboardSession {
    fn open() -> Option<Self> {
        for _ in 0..CLIPBOARD_OPEN_ATTEMPTS {
            if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
                return Some(Self);
            }
            thread::sleep(Duration::from_millis(5));
        }
        None
    }
}

#[cfg(windows)]
impl Drop for ClipboardSession {
    fn drop(&mut self) {
        unsafe { CloseClipboard() };
    }
}

#[cfg(windows)]
fn dib_u32(data: &[u8], offset: usize) -> Option<u32> {
    data.get(offset..offset + 4)
        .map(|slice| u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

#[cfg(windows)]
fn dib_i32(data: &[u8], offset: usize) -> Option<i32> {
    dib_u32(data, offset).map(|value| value as i32)
}

#[cfg(windows)]
fn dib_u16(data: &[u8], offset: usize) -> Option<u16> {
    data.get(offset..offset + 2)
        .map(|slice| u16::from_le_bytes([slice[0], slice[1]]))
}

/// Extract one channel from a packed pixel using its BI_BITFIELDS mask, scaled to 8 bits.
#[cfg(windows)]
fn channel_from_mask(pixel: u32, mask: u32) -> u8 {
    if mask == 0 {
        return 0;
    }
    let shift = mask.trailing_zeros();
    let bits = (mask >> shift).count_ones();
    let value = (pixel & mask) >> shift;
    if bits >= 8 {
        return (value >> (bits - 8)) as u8;
    }
    let max = (1u32 << bits) - 1;
    ((value * 255 + max / 2) / max) as u8
}

/// Decode a packed DIB (BITMAPINFOHEADER / V4 / V5 followed by pixels) into top-down RGBA.
///
/// Only the 24- and 32-bit uncompressed layouts real clipboard producers use are handled;
/// anything else returns `None` and is left to arboard.
#[cfg(windows)]
fn decode_dib(data: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let header_size = dib_u32(data, 0)? as usize;
    if header_size < 40 || header_size > data.len() {
        return None;
    }
    let width_raw = dib_i32(data, 4)?;
    let height_raw = dib_i32(data, 8)?;
    let bit_count = dib_u16(data, 14)?;
    let compression = dib_u32(data, 16)?;
    let colors_used = dib_u32(data, 32)?;

    if width_raw <= 0 || height_raw == 0 || (bit_count != 24 && bit_count != 32) {
        return None;
    }
    let width = u32::try_from(width_raw).ok()?;
    // A negative height means the rows are already stored top-to-bottom.
    let top_down = height_raw < 0;
    let height = height_raw.unsigned_abs();
    if width == 0 || height == 0 || width > DIB_MAX_EDGE || height > DIB_MAX_EDGE {
        return None;
    }

    let (masks, extra_mask_bytes) = match compression {
        BI_BITFIELDS => {
            if header_size >= 108 {
                // A V4/V5 header carries the masks inside itself, with nothing between the
                // header and the pixels. (Mis-handling exactly this is what makes the
                // `image` crate's BMP decoder — and therefore arboard — fail here.)
                (
                    Some([
                        dib_u32(data, 40)?,
                        dib_u32(data, 44)?,
                        dib_u32(data, 48)?,
                        dib_u32(data, 52)?,
                    ]),
                    0usize,
                )
            } else {
                // A plain BITMAPINFOHEADER keeps them in the 12 bytes that follow it.
                (
                    Some([
                        dib_u32(data, header_size)?,
                        dib_u32(data, header_size + 4)?,
                        dib_u32(data, header_size + 8)?,
                        0,
                    ]),
                    12usize,
                )
            }
        }
        BI_RGB => (None, 0usize),
        // RLE, or a JPEG/PNG smuggled inside a DIB.
        _ => return None,
    };

    let palette_bytes = (colors_used as usize).saturating_mul(4);
    let pixel_offset = header_size
        .checked_add(extra_mask_bytes)?
        .checked_add(palette_bytes)?;
    let stride = (width as usize)
        .checked_mul(bit_count as usize)?
        .checked_add(31)?
        / 32
        * 4;
    let needed = stride.checked_mul(height as usize)?;
    if data.len() < pixel_offset.checked_add(needed)? {
        return None;
    }
    let pixels = &data[pixel_offset..pixel_offset + needed];

    let mut rgba = vec![0u8; (width as usize).checked_mul(height as usize)?.checked_mul(4)?];
    let mut any_alpha = false;
    for row in 0..height as usize {
        let source_row = if top_down {
            row
        } else {
            height as usize - 1 - row
        };
        let source = &pixels[source_row * stride..source_row * stride + stride];
        for column in 0..width as usize {
            let destination = (row * width as usize + column) * 4;
            if bit_count == 24 {
                let offset = column * 3;
                rgba[destination] = source[offset + 2];
                rgba[destination + 1] = source[offset + 1];
                rgba[destination + 2] = source[offset];
                rgba[destination + 3] = 255;
                continue;
            }
            let offset = column * 4;
            let (red, green, blue, alpha) = match masks {
                Some([red_mask, green_mask, blue_mask, alpha_mask]) => {
                    let packed = u32::from_le_bytes([
                        source[offset],
                        source[offset + 1],
                        source[offset + 2],
                        source[offset + 3],
                    ]);
                    (
                        channel_from_mask(packed, red_mask),
                        channel_from_mask(packed, green_mask),
                        channel_from_mask(packed, blue_mask),
                        channel_from_mask(packed, alpha_mask),
                    )
                }
                // BI_RGB documents the top byte as unused, but every browser-class producer
                // puts real transparency there, so read it and let the all-zero check below
                // sort out the ones that meant it as padding.
                None => (
                    source[offset + 2],
                    source[offset + 1],
                    source[offset],
                    source[offset + 3],
                ),
            };
            rgba[destination] = red;
            rgba[destination + 1] = green;
            rgba[destination + 2] = blue;
            rgba[destination + 3] = alpha;
            if alpha != 0 {
                any_alpha = true;
            }
        }
    }

    // A 32-bit DIB whose alpha plane is entirely zero came from a producer that never filled
    // the byte in. Taking it literally would store a fully invisible image, so treat it as
    // opaque — a genuinely all-transparent image carries no information to lose either way.
    if bit_count == 32 && !any_alpha {
        for pixel in rgba.chunks_exact_mut(4) {
            pixel[3] = 255;
        }
    }
    Some((width, height, rgba))
}

/// Read the clipboard's bitmap straight from CF_DIBV5/CF_DIB.
///
/// This exists because arboard decodes CF_DIBV5 through the `image` crate's BMP decoder,
/// which — for a V4/V5 header using BI_BITFIELDS — seeks 12 bytes past the header looking for
/// a repeated RGB mask triple that is only present after a plain BITMAPINFOHEADER. The pixel
/// data is then read out of alignment and the decode fails. That header shape is exactly what
/// browsers and screenshot tools publish for an image carrying an alpha channel, and arboard's
/// error for it is indistinguishable from "no image here", so such copies were silently
/// dropped instead of entering the history.
#[cfg(windows)]
fn read_clipboard_dib() -> Option<(u32, u32, Vec<u8>)> {
    let _session = ClipboardSession::open()?;
    for format in [CF_DIBV5, CF_DIB] {
        if unsafe { IsClipboardFormatAvailable(format) } == 0 {
            continue;
        }
        let handle = unsafe { GetClipboardData(format) };
        if handle.is_null() {
            continue;
        }
        let size = unsafe { GlobalSize(handle) };
        if size < 40 {
            continue;
        }
        let pointer = unsafe { GlobalLock(handle) } as *const u8;
        if pointer.is_null() {
            continue;
        }
        let decoded = decode_dib(unsafe { std::slice::from_raw_parts(pointer, size) });
        unsafe { GlobalUnlock(handle) };
        if decoded.is_some() {
            return decoded;
        }
    }
    None
}

#[cfg(not(windows))]
fn read_clipboard_dib() -> Option<(u32, u32, Vec<u8>)> {
    None
}

fn read_clipboard_raw(app: &AppHandle, sequence: u32) -> ClipboardRead {
    let Ok(mut clipboard) = Clipboard::new() else {
        return ClipboardRead::Unreadable;
    };
    let created_at = now_ms();
    // Set whenever a read failed because another process had the clipboard open, so the
    // caller can tell "nothing to capture" apart from "come back and try again".
    let mut occupied = false;

    match clipboard.get_image() {
        Ok(image) => {
            return ClipboardRead::Item(Box::new(RawClipboardItem::Image {
                sequence,
                created_at,
                width: image.width,
                height: image.height,
                bytes: image.bytes.into_owned(),
            }))
        }
        // No bitmap at all — the common case for text; nothing to fall back to.
        Err(arboard::Error::ContentNotAvailable) => {}
        Err(arboard::Error::ClipboardOccupied) => occupied = true,
        // A bitmap IS there and arboard could not turn it into an image. Decode it ourselves
        // rather than losing the copy.
        Err(_) => {
            if let Some((width, height, bytes)) = read_clipboard_dib() {
                return ClipboardRead::Item(Box::new(RawClipboardItem::Image {
                    sequence,
                    created_at,
                    width: width as usize,
                    height: height as usize,
                    bytes,
                }));
            }
        }
    }

    match clipboard.get().file_list() {
        Ok(paths) => {
            if let Some(item) = raw_file_image_from_paths(app, paths, sequence) {
                return ClipboardRead::Item(Box::new(item));
            }
        }
        Err(arboard::Error::ClipboardOccupied) => occupied = true,
        Err(_) => {}
    }

    match clipboard.get_text() {
        Ok(text) => {
            if let Some(item) = text_item(sequence, created_at, text) {
                return ClipboardRead::Item(Box::new(RawClipboardItem::Text {
                    sequence,
                    created_at,
                    text: item.text.unwrap_or_default(),
                }));
            }
        }
        Err(arboard::Error::ClipboardOccupied) => occupied = true,
        Err(_) => {}
    }

    if occupied {
        ClipboardRead::Unreadable
    } else {
        ClipboardRead::Nothing
    }
}

fn raw_to_clipboard_item(app: &AppHandle, raw: RawClipboardItem) -> Option<ClipboardItem> {
    match raw {
        RawClipboardItem::Image {
            sequence,
            created_at,
            width,
            height,
            bytes,
        } => image_item_from_rgba_with_timestamp(app, sequence, created_at, width, height, bytes)
            .ok(),
        RawClipboardItem::Text {
            sequence,
            created_at,
            text,
        } => text_item(sequence, created_at, text),
        RawClipboardItem::FileImage(item) => Some(item),
    }
}

fn push_pending_item(app: &AppHandle, item: ClipboardItem) {
    let state = app.state::<AppState>();
    if let Ok(mut pending) = state.pending_items.lock() {
        pending.push_back(item);
        while pending.len() > 96 {
            pending.pop_front();
        }
    };
    trim_history_cache(app);
    let _ = app.emit("clipboard-items-ready", ());
}

fn remember_own_clipboard_sequence(app: &AppHandle) {
    let sequence = clipboard_sequence();
    if sequence == 0 {
        return;
    }
    let state = app.state::<AppState>();
    if let Ok(mut ignored) = state.ignored_sequences.lock() {
        ignored.push_back(sequence);
        while ignored.len() > 16 {
            ignored.pop_front();
        }
    };
}

fn take_ignored_clipboard_sequence(app: &AppHandle, sequence: u32) -> bool {
    let state = app.state::<AppState>();
    let Ok(mut ignored) = state.ignored_sequences.lock() else {
        return false;
    };
    if let Some(index) = ignored.iter().position(|value| *value == sequence) {
        ignored.remove(index);
        return true;
    }
    false
}

fn process_raw_clipboard_items(app: AppHandle, receiver: Receiver<RawClipboardItem>) {
    for raw in receiver {
        if let Some(item) = raw_to_clipboard_item(&app, raw) {
            push_pending_item(&app, item);
        }
    }
}

fn start_clipboard_watcher(app: AppHandle) {
    let (sender, receiver) = mpsc::channel::<RawClipboardItem>();
    let worker_app = app.clone();

    thread::spawn(move || process_raw_clipboard_items(worker_app, receiver));
    thread::spawn(move || clipboard_watch_loop(app, sender));
}

/// Read the current clipboard revision once and forward it, retrying briefly through transient
/// read failures (another process can still hold the clipboard open right after it changed).
/// `last_sequence` is updated so the same revision is never captured twice.
///
/// Returns `false` when the revision is still unread because the clipboard stayed locked. The
/// caller must then arrange to come back to it — `last_sequence` is deliberately left alone in
/// that case, so nothing marks the revision as dealt with.
fn capture_clipboard_revision(
    app: &AppHandle,
    sender: &mpsc::Sender<RawClipboardItem>,
    last_sequence: &mut u32,
) -> bool {
    let sequence = clipboard_sequence();
    if sequence == 0 || sequence == *last_sequence {
        return true;
    }

    let state = app.state::<AppState>();
    if state.own_clipboard_write_active.load(Ordering::SeqCst)
        || take_ignored_clipboard_sequence(app, sequence)
    {
        *last_sequence = sequence;
        return true;
    }

    for attempt in 0..CLIPBOARD_READ_ATTEMPTS {
        match read_clipboard_raw(app, sequence) {
            ClipboardRead::Item(item) => {
                let _ = sender.send(*item);
                *last_sequence = sequence;
                return true;
            }
            ClipboardRead::Nothing => {
                *last_sequence = sequence;
                return true;
            }
            ClipboardRead::Unreadable => {
                if attempt + 1 < CLIPBOARD_READ_ATTEMPTS {
                    thread::sleep(CLIPBOARD_READ_RETRY_DELAY);
                }
            }
        }
    }
    false
}

#[cfg(windows)]
struct ClipboardListenerContext {
    app: AppHandle,
    sender: mpsc::Sender<RawClipboardItem>,
    last_sequence: u32,
    /// How many times in a row we have rescheduled a read of the same unread revision.
    recheck_rounds: u32,
}

/// Run a capture and, if the clipboard was locked throughout, arrange to come back to it.
///
/// The listener is purely event-driven, so a revision that is dropped here is dropped for
/// good: no later notification arrives for content that is already on the clipboard. A
/// one-shot timer on the listener window is what keeps that from happening, and it is only
/// ever armed while a read is outstanding, so an idle app still costs nothing.
#[cfg(windows)]
fn capture_or_reschedule(hwnd: HWND, context: &mut ClipboardListenerContext) {
    let settled = capture_clipboard_revision(
        &context.app,
        &context.sender,
        &mut context.last_sequence,
    );
    if settled {
        context.recheck_rounds = 0;
        unsafe { KillTimer(hwnd, CLIPBOARD_RECHECK_TIMER_ID) };
        return;
    }
    if context.recheck_rounds >= CLIPBOARD_RECHECK_MAX_ROUNDS {
        // Whoever holds the clipboard is not letting go. Stop retrying so the listener
        // thread is free for the next real change.
        context.recheck_rounds = 0;
        context.last_sequence = clipboard_sequence();
        unsafe { KillTimer(hwnd, CLIPBOARD_RECHECK_TIMER_ID) };
        return;
    }
    context.recheck_rounds += 1;
    unsafe {
        SetTimer(
            hwnd,
            CLIPBOARD_RECHECK_TIMER_ID,
            CLIPBOARD_RECHECK_DELAY_MS,
            None,
        )
    };
}

/// Event-driven clipboard capture: a hidden message-only window subscribes via
/// `AddClipboardFormatListener` and wakes only on `WM_CLIPBOARDUPDATE`. This replaces the old
/// fixed-interval poll so the app uses no CPU while the clipboard is idle — important for a
/// passive, always-on app.
#[cfg(windows)]
fn clipboard_watch_loop(app: AppHandle, sender: mpsc::Sender<RawClipboardItem>) {
    let class_name: Vec<u16> = "ClipboardManagerClipboardListener\0"
        .encode_utf16()
        .collect();

    unsafe {
        let hinstance = GetModuleHandleW(std::ptr::null());

        let mut wnd_class: WNDCLASSW = std::mem::zeroed();
        wnd_class.lpfnWndProc = Some(clipboard_wndproc);
        wnd_class.hInstance = hinstance;
        wnd_class.lpszClassName = class_name.as_ptr();
        RegisterClassW(&wnd_class);

        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            class_name.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            std::ptr::null_mut(),
            hinstance,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            return;
        }

        let context = Box::new(ClipboardListenerContext {
            app,
            sender,
            last_sequence: 0,
            recheck_rounds: 0,
        });
        let context = Box::into_raw(context);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, context as isize);

        if AddClipboardFormatListener(hwnd) == 0 {
            // Could not subscribe; drop the context and bail. The window stays inert.
            drop(Box::from_raw(context));
            return;
        }

        // Capture whatever is already on the clipboard at launch (no update event fires for it).
        capture_or_reschedule(hwnd, &mut *context);

        let mut message: MSG = std::mem::zeroed();
        while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }

        // The pump only ends on WM_QUIT, which this app never posts; reclaim the context anyway.
        drop(Box::from_raw(context));
    }
}

#[cfg(windows)]
unsafe extern "system" fn clipboard_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CLIPBOARDUPDATE => {
            let context =
                GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut ClipboardListenerContext;
            if let Some(context) = context.as_mut() {
                capture_or_reschedule(hwnd, context);
            }
            0
        }
        // Only ever armed by `capture_or_reschedule` after a read found the clipboard locked.
        WM_TIMER if wparam == CLIPBOARD_RECHECK_TIMER_ID => {
            KillTimer(hwnd, CLIPBOARD_RECHECK_TIMER_ID);
            let context =
                GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut ClipboardListenerContext;
            if let Some(context) = context.as_mut() {
                capture_or_reschedule(hwnd, context);
            }
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

#[cfg(not(windows))]
fn clipboard_watch_loop(_app: AppHandle, _sender: mpsc::Sender<RawClipboardItem>) {
    // Clipboard capture is Windows-only (it relies on the Win32 clipboard sequence number and
    // change notifications). On other platforms the watcher is inert.
}

fn clipboard_retry_delay() {
    thread::sleep(CLIPBOARD_COPY_RETRY_DELAY);
}

fn copy_text_to_clipboard(text: String) -> Result<(), String> {
    let mut last_error = None;

    for _ in 0..CLIPBOARD_COPY_ATTEMPTS {
        match Clipboard::new() {
            Ok(mut clipboard) => match clipboard.set_text(text.clone()) {
                Ok(()) => {
                    clipboard_retry_delay();
                    match Clipboard::new().and_then(|mut clipboard| clipboard.get_text()) {
                        Ok(current) if current == text => return Ok(()),
                        Ok(_) => {
                            last_error =
                                Some("Clipboard did not contain the copied text.".to_string());
                        }
                        Err(error) => {
                            last_error = Some(format!("Failed to verify copied text: {error}"))
                        }
                    }
                }
                Err(error) => last_error = Some(format!("Failed to copy text: {error}")),
            },
            Err(error) => last_error = Some(format!("Failed to access clipboard: {error}")),
        }
        clipboard_retry_delay();
    }

    Err(last_error.unwrap_or_else(|| "Failed to copy text.".to_string()))
}

fn copied_image_matches(width: usize, height: usize, bytes: &[u8]) -> Result<bool, String> {
    let mut clipboard =
        Clipboard::new().map_err(|error| format!("Failed to access clipboard: {error}"))?;
    let image = clipboard
        .get_image()
        .map_err(|error| format!("Failed to verify copied image: {error}"))?;

    Ok(image.width == width && image.height == height && image.bytes.as_ref() == bytes)
}

fn copy_image_to_clipboard(file_path: &Path) -> Result<(), String> {
    let image = ImageReader::open(file_path)
        .map_err(|error| format!("Failed to open image: {error}"))?
        .with_guessed_format()
        .map_err(|error| format!("Failed to detect image format: {error}"))?
        .decode()
        .map_err(|error| format!("Failed to decode image: {error}"))?
        .to_rgba8();

    let width = usize::try_from(image.width()).map_err(|_| "Image is too wide.".to_string())?;
    let height = usize::try_from(image.height()).map_err(|_| "Image is too tall.".to_string())?;
    let bytes = image.into_raw();
    let mut last_error = None;

    for _ in 0..CLIPBOARD_COPY_ATTEMPTS {
        let data = ImageData {
            width,
            height,
            bytes: Cow::Borrowed(bytes.as_slice()),
        };

        match Clipboard::new() {
            Ok(mut clipboard) => match clipboard.set_image(data) {
                Ok(()) => {
                    clipboard_retry_delay();
                    match copied_image_matches(width, height, &bytes) {
                        Ok(true) => return Ok(()),
                        Ok(false) => {
                            last_error =
                                Some("Clipboard did not contain the copied image.".to_string());
                        }
                        Err(error) => last_error = Some(error),
                    }
                }
                Err(error) => last_error = Some(format!("Failed to copy image: {error}")),
            },
            Err(error) => last_error = Some(format!("Failed to access clipboard: {error}")),
        }
        clipboard_retry_delay();
    }

    Err(last_error.unwrap_or_else(|| "Failed to copy image.".to_string()))
}

fn current_logical_window_state(window: &WebviewWindow) -> Result<WindowState, String> {
    let scale = window
        .scale_factor()
        .map_err(|error| format!("Failed to read window scale factor: {error}"))?;
    let position = window
        .outer_position()
        .map_err(|error| format!("Failed to read window position: {error}"))?;
    let size = window
        .inner_size()
        .map_err(|error| format!("Failed to read window size: {error}"))?;
    Ok(WindowState {
        x: (f64::from(position.x) / scale).round() as i32,
        y: (f64::from(position.y) / scale).round() as i32,
        width: (f64::from(size.width) / scale).round() as u32,
        height: (f64::from(size.height) / scale).round() as u32,
    })
}

fn window_bounds_from_state(state: &WindowState) -> WindowBounds {
    WindowBounds {
        x: state.x,
        y: state.y,
        width: state.width,
        height: state.height,
    }
}

/// Relax (or restore) the native minimum window width to match whether the displayers column
/// is showing. This only changes the *constraint*; it never resizes the window itself, so
/// disabling displayers makes a smaller window possible without forcing one.
fn apply_window_min_size(window: &WebviewWindow, displayers_enabled: bool) -> Result<(), String> {
    let width = if displayers_enabled {
        MIN_WINDOW_WIDTH_WITH_DISPLAYERS
    } else {
        MIN_WINDOW_WIDTH_WITHOUT_DISPLAYERS
    };
    window
        .set_min_size(Some(Size::Logical(LogicalSize {
            width,
            height: MIN_WINDOW_HEIGHT,
        })))
        .map_err(|error| format!("Failed to set window minimum size: {error}"))
}

#[cfg(windows)]
fn set_window_corner_preference(window: &WebviewWindow, preference: u32) {
    let Ok(hwnd) = window.hwnd() else {
        return;
    };
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd.0 as _,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            (&preference as *const u32).cast(),
            std::mem::size_of_val(&preference) as u32,
        );
    }
}

#[cfg(windows)]
fn square_window_corners(window: &WebviewWindow) {
    set_window_corner_preference(window, DWMWCP_DONOTROUND);
}

#[cfg(not(windows))]
fn square_window_corners(_window: &WebviewWindow) {}

/// True when the window is filling a region of the screen rather than floating freely —
/// maximized, or snapped by drag-to-edge / Win+Arrow / Snap Layouts.
///
/// This is measured geometrically, against the monitor's work area, because Windows exposes no
/// "is snapped" flag. The tempting proxy — a `WINDOWPLACEMENT.rcNormalPosition` that still
/// holds the pre-snap size — does not work: measured on this machine, snapping updates the
/// restore rect to match the snapped rect, so it never diverges. What every snap layout does
/// share is that the window sits flush against the work-area edges on two or more sides while
/// covering a large share of it, and that is what is tested here.
#[cfg(windows)]
fn is_window_snapped(hwnd: HWND) -> bool {
    unsafe {
        if IsZoomed(hwnd) != 0 {
            return true;
        }
        let mut frame: RECT = std::mem::zeroed();
        let has_frame_bounds = DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&mut frame as *mut RECT).cast(),
            std::mem::size_of::<RECT>() as u32,
        ) == 0;
        if !has_frame_bounds && GetWindowRect(hwnd, &mut frame) == 0 {
            return false;
        }

        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        if monitor.is_null() {
            return false;
        }
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(monitor, &mut info) == 0 {
            return false;
        }
        let work = info.rcWork;

        let flush_edges = [
            frame.left - work.left,
            frame.top - work.top,
            frame.right - work.right,
            frame.bottom - work.bottom,
        ]
        .iter()
        .filter(|delta| delta.abs() <= SNAP_EDGE_TOLERANCE)
        .count();
        if flush_edges < 2 {
            return false;
        }

        let spans_width =
            (frame.right - frame.left) * 100 >= (work.right - work.left) * SNAP_SPAN_PERCENT;
        let spans_height =
            (frame.bottom - frame.top) * 100 >= (work.bottom - work.top) * SNAP_SPAN_PERCENT;
        spans_width || spans_height
    }
}

/// Floating image windows get Windows 11 rounded corners while they float free, and square
/// corners the moment they are snapped — so snapped images tile flush against each other and
/// the screen edges instead of leaving rounded notches.
///
/// `last_preference` caches what was last handed to DWM (0 = nothing yet) so the per-pixel
/// `Moved` events of a window drag don't turn into a stream of redundant DWM calls.
#[cfg(windows)]
fn apply_floating_image_corners(window: &WebviewWindow, last_preference: &AtomicU32) {
    let Ok(hwnd) = window.hwnd() else {
        return;
    };
    let hwnd = hwnd.0 as HWND;
    // A minimized window reports a placeholder rect that would read as snapped; leave the
    // current preference alone and re-evaluate when it is restored.
    if unsafe { IsIconic(hwnd) } != 0 {
        return;
    }
    let preference = if is_window_snapped(hwnd) {
        DWMWCP_DONOTROUND
    } else {
        DWMWCP_ROUND
    };
    if last_preference.swap(preference, Ordering::Relaxed) == preference {
        return;
    }
    set_window_corner_preference(window, preference);
}

#[cfg(not(windows))]
fn apply_floating_image_corners(_window: &WebviewWindow, _last_preference: &AtomicU32) {}

fn expand_borderless_edges(bounds: &WindowState) -> WindowState {
    let expand = u32::try_from(BORDERLESS_EDGE_EXPAND).unwrap_or(0);
    WindowState {
        x: bounds.x.saturating_sub(BORDERLESS_EDGE_EXPAND),
        y: bounds.y.saturating_sub(BORDERLESS_EDGE_EXPAND),
        width: bounds.width.saturating_add(expand.saturating_mul(2)),
        height: bounds.height.saturating_add(expand.saturating_mul(2)),
    }
}

fn shrink_borderless_edges(bounds: &WindowState) -> WindowState {
    let shrink = u32::try_from(BORDERLESS_EDGE_EXPAND).unwrap_or(0);
    WindowState {
        x: bounds.x.saturating_add(BORDERLESS_EDGE_EXPAND),
        y: bounds.y.saturating_add(BORDERLESS_EDGE_EXPAND),
        width: bounds.width.saturating_sub(shrink.saturating_mul(2)).max(1),
        height: bounds
            .height
            .saturating_sub(shrink.saturating_mul(2))
            .max(1),
    }
}

fn set_window_bounds(
    window: &WebviewWindow,
    bounds: &WindowState,
    expand_edges: bool,
) -> Result<(), String> {
    if bounds.width == 0 || bounds.height == 0 {
        return Ok(());
    }
    let adjusted;
    let bounds = if expand_edges {
        adjusted = expand_borderless_edges(bounds);
        &adjusted
    } else {
        bounds
    };
    // Set position first so the window lands on its target monitor, then size (resolved at
    // that monitor's scale factor), then position again to finalize — applying the size can
    // nudge the window. This is what keeps restore correct across mixed-DPI monitors.
    window
        .set_position(Position::Logical(LogicalPosition {
            x: f64::from(bounds.x),
            y: f64::from(bounds.y),
        }))
        .map_err(|error| format!("Failed to restore window position: {error}"))?;
    window
        .set_size(Size::Logical(LogicalSize {
            width: f64::from(bounds.width),
            height: f64::from(bounds.height),
        }))
        .map_err(|error| format!("Failed to restore window size: {error}"))?;
    window
        .set_position(Position::Logical(LogicalPosition {
            x: f64::from(bounds.x),
            y: f64::from(bounds.y),
        }))
        .map_err(|error| format!("Failed to restore final window position: {error}"))
}

#[tauri::command]
fn load_settings(app: AppHandle) -> Settings {
    load_settings_inner(&app)
}

#[tauri::command]
fn save_settings(app: AppHandle, settings: Settings) -> Result<Settings, String> {
    let settings = normalize_settings(settings);
    save_settings_inner(&app, &settings)?;
    Ok(settings)
}

fn persist_window_state(
    app: &AppHandle,
    window: &WebviewWindow,
) -> Result<Option<WindowBounds>, String> {
    // Skip while hidden or minimized: inner_size() can report a transient/tiny size that
    // would otherwise shrink the window on the next launch.
    if window.is_minimized().unwrap_or(false) || !window.is_visible().unwrap_or(true) {
        return Ok(None);
    }
    let mut state = current_logical_window_state(window)?;
    if state.width == 0 || state.height == 0 {
        return Ok(None);
    }
    let mut settings = load_settings_inner(app);
    if settings.expand_borderless_edges {
        state = shrink_borderless_edges(&state);
    }
    let bounds = window_bounds_from_state(&state);
    settings.window = Some(state);
    save_settings_inner(app, &settings)?;
    Ok(Some(bounds))
}

#[tauri::command]
fn save_window_state(
    app: AppHandle,
    window: WebviewWindow,
) -> Result<Option<WindowBounds>, String> {
    persist_window_state(&app, &window)
}

#[tauri::command]
fn window_show(window: WebviewWindow, state: tauri::State<'_, AppState>) -> Result<(), String> {
    state.window_shown.store(true, Ordering::SeqCst);
    window.show().map_err(|error| error.to_string())
}

#[tauri::command]
fn set_displayers_enabled_window_constraint(
    window: WebviewWindow,
    enabled: bool,
) -> Result<(), String> {
    apply_window_min_size(&window, enabled)
}

#[tauri::command]
fn adjust_window_borderless_edges(window: WebviewWindow, expand: bool) -> Result<(), String> {
    let bounds = current_logical_window_state(&window)?;
    let adjusted = if expand {
        expand_borderless_edges(&bounds)
    } else {
        shrink_borderless_edges(&bounds)
    };

    set_window_bounds(&window, &adjusted, false)
}

#[tauri::command]
fn drain_clipboard_items(state: tauri::State<'_, AppState>) -> Vec<ClipboardItem> {
    let Ok(mut pending) = state.pending_items.lock() else {
        return Vec::new();
    };
    pending.drain(..).collect()
}

#[tauri::command]
fn clear_history(app: AppHandle, state: tauri::State<'_, AppState>) {
    if let Ok(mut pending) = state.pending_items.lock() {
        pending.clear();
    }
    clean_history_cache(&app);
    if let Ok(path) = hidden_history_path(&app) {
        let _ = fs::remove_file(path);
    }
}

#[tauri::command]
fn save_hidden_history(app: AppHandle, items: Vec<ClipboardItem>) -> Result<(), String> {
    let path = hidden_history_path(&app)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("Failed to create hidden history directory: {error}"))?;
    }
    let data = serde_json::to_string_pretty(&items)
        .map_err(|error| format!("Failed to serialize hidden history: {error}"))?;
    fs::write(path, data).map_err(|error| format!("Failed to save hidden history: {error}"))
}

/// Whether a saved history entry is still usable. An image entry points at a file in the
/// clipboard cache, and that cache is wiped at startup and trimmed while running, so a saved
/// entry easily outlives the file it names. Restoring one of those puts a tile in the grid
/// that can never render and can never be copied back, so they are dropped on the way in.
fn history_item_file_present(item: &ClipboardItem) -> bool {
    match item.file_path.as_deref() {
        Some(path) => Path::new(path).is_file(),
        None => true,
    }
}

#[tauri::command]
fn load_hidden_history(app: AppHandle) -> Result<Vec<ClipboardItem>, String> {
    let path = hidden_history_path(&app)?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let data = fs::read_to_string(path)
        .map_err(|error| format!("Failed to read hidden history: {error}"))?;
    let items = serde_json::from_str::<Vec<ClipboardItem>>(&data)
        .map_err(|error| format!("Failed to parse hidden history: {error}"))?;
    Ok(items
        .into_iter()
        .filter(history_item_file_present)
        .collect())
}

/// Drop saved hidden-history entries whose cached images are gone. Called right after the
/// startup cache wipe, which is what orphans them in the first place.
fn prune_hidden_history(app: &AppHandle) {
    let Ok(path) = hidden_history_path(app) else {
        return;
    };
    if !path.exists() {
        return;
    }
    let Ok(data) = fs::read_to_string(&path) else {
        return;
    };
    let Ok(items) = serde_json::from_str::<Vec<ClipboardItem>>(&data) else {
        let _ = fs::remove_file(&path);
        return;
    };
    let total = items.len();
    let kept: Vec<ClipboardItem> = items
        .into_iter()
        .filter(history_item_file_present)
        .collect();
    if kept.len() == total {
        return;
    }
    if kept.is_empty() {
        let _ = fs::remove_file(&path);
        return;
    }
    if let Ok(data) = serde_json::to_string_pretty(&kept) {
        let _ = fs::write(&path, data);
    }
}

#[tauri::command]
fn copy_item_to_clipboard(
    app: AppHandle,
    kind: String,
    text: Option<String>,
    file_path: Option<String>,
) -> Result<(), String> {
    let state = app.state::<AppState>();
    state
        .own_clipboard_write_active
        .store(true, Ordering::SeqCst);
    let result = match kind.as_str() {
        "text" => match text {
            Some(text) => copy_text_to_clipboard(text),
            None => Err("No text content was supplied.".to_string()),
        },
        "image" => match file_path {
            Some(file_path) => copy_image_to_clipboard(Path::new(&file_path)),
            None => Err("No image path was supplied.".to_string()),
        },
        _ => Err("Unsupported clipboard item type.".to_string()),
    };
    // Record our own write's sequence number *before* clearing the in-progress flag, so the
    // listener can never observe a window where the flag is down but the sequence isn't yet
    // ignored — which would make it re-capture the item we just copied.
    if result.is_ok() {
        remember_own_clipboard_sequence(&app);
    }
    state
        .own_clipboard_write_active
        .store(false, Ordering::SeqCst);
    result
}

#[tauri::command]
fn save_image_as_default(app: AppHandle, file_path: String) -> Result<String, String> {
    let source = PathBuf::from(file_path);
    let image = ImageReader::open(&source)
        .map_err(|error| format!("Failed to open image: {error}"))?
        .with_guessed_format()
        .map_err(|error| format!("Failed to detect image format: {error}"))?
        .decode()
        .map_err(|error| format!("Failed to decode image: {error}"))?
        .to_rgba8();

    let dir = default_images_dir(&app)?;
    fs::create_dir_all(&dir)
        .map_err(|error| format!("Failed to create default image directory: {error}"))?;
    let path = dir.join(format!("default-{}.png", now_ms()));
    image
        .save_with_format(&path, ImageFormat::Png)
        .map_err(|error| format!("Failed to save default image: {error}"))?;
    Ok(path.to_string_lossy().to_string())
}

fn path_is_within(dir: &Path, candidate: &Path) -> bool {
    match (dir.canonicalize(), candidate.canonicalize()) {
        (Ok(dir), Ok(candidate)) => candidate.starts_with(dir),
        _ => false,
    }
}

/// Paths of every image in the shared default-image pool, newest first.
#[tauri::command]
fn list_default_images(app: AppHandle) -> Result<Vec<String>, String> {
    let dir = default_images_dir(&app)?;
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut entries: Vec<(u128, String)> = fs::read_dir(&dir)
        .map_err(|error| format!("Failed to read default images: {error}"))?
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_file() || !is_image_path(&path) {
                return None;
            }
            let modified = entry
                .metadata()
                .ok()
                .and_then(|meta| meta.modified().ok())
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|duration| duration.as_millis())
                .unwrap_or_default();
            Some((modified, path.to_string_lossy().to_string()))
        })
        .collect();
    entries.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(entries.into_iter().map(|(_, path)| path).collect())
}

/// Copy a chosen image into the shared pool verbatim (preserving its format).
#[tauri::command]
fn add_default_image(app: AppHandle, file_path: String) -> Result<String, String> {
    let source = PathBuf::from(&file_path);
    if !source.is_file() || !is_image_path(&source) {
        return Err("Selected file is not a supported image.".to_string());
    }
    let dir = default_images_dir(&app)?;
    fs::create_dir_all(&dir)
        .map_err(|error| format!("Failed to create default image directory: {error}"))?;
    let ext = source
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("png")
        .to_lowercase();
    let dest = dir.join(format!("default-{}.{ext}", now_ms()));
    fs::copy(&source, &dest).map_err(|error| format!("Failed to add default image: {error}"))?;
    Ok(dest.to_string_lossy().to_string())
}

/// Delete an image from the shared pool. Refuses paths outside the pool directory.
#[tauri::command]
fn remove_default_image(app: AppHandle, file_path: String) -> Result<(), String> {
    let dir = default_images_dir(&app)?;
    let target = PathBuf::from(&file_path);
    if !path_is_within(&dir, &target) {
        return Err("Refusing to delete a file outside the default image pool.".to_string());
    }
    fs::remove_file(&target).map_err(|error| format!("Failed to remove default image: {error}"))
}

/// Save whatever image is currently on the clipboard (raw bitmap or a copied image file)
/// into the shared pool. Returns the new path, or `None` if the clipboard holds no image.
#[tauri::command]
fn paste_default_image(app: AppHandle) -> Result<Option<String>, String> {
    let dir = default_images_dir(&app)?;
    fs::create_dir_all(&dir)
        .map_err(|error| format!("Failed to create default image directory: {error}"))?;

    let mut clipboard =
        Clipboard::new().map_err(|error| format!("Failed to access clipboard: {error}"))?;

    // A raw bitmap (e.g. a screenshot or a copied image region).
    if let Ok(image) = clipboard.get_image() {
        let (raw_width, raw_height) = (image.width, image.height);
        let width =
            u32::try_from(raw_width).map_err(|_| "Clipboard image is too wide.".to_string())?;
        let height =
            u32::try_from(raw_height).map_err(|_| "Clipboard image is too tall.".to_string())?;
        let bytes = image.bytes.into_owned();
        if bytes.len() == raw_width.saturating_mul(raw_height).saturating_mul(4) {
            let path = dir.join(format!("default-{}.png", now_ms()));
            let file = File::create(&path)
                .map_err(|error| format!("Failed to create default image file: {error}"))?;
            let encoder =
                PngEncoder::new_with_quality(file, CompressionType::Fast, FilterType::NoFilter);
            encoder
                .write_image(&bytes, width, height, ColorType::Rgba8.into())
                .map_err(|error| format!("Failed to save pasted image: {error}"))?;
            return Ok(Some(path.to_string_lossy().to_string()));
        }
    }

    // A copied image file on disk.
    if let Ok(paths) = clipboard.get().file_list() {
        if let Some(source) = paths
            .into_iter()
            .find(|path| path.is_file() && is_image_path(path))
        {
            let ext = source
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or("png")
                .to_lowercase();
            let dest = dir.join(format!("default-{}.{ext}", now_ms()));
            fs::copy(&source, &dest)
                .map_err(|error| format!("Failed to add pasted image: {error}"))?;
            return Ok(Some(dest.to_string_lossy().to_string()));
        }
    }

    Ok(None)
}

#[tauri::command]
fn window_minimize(window: WebviewWindow) -> Result<(), String> {
    window.minimize().map_err(|error| error.to_string())
}

#[tauri::command]
fn window_close(window: WebviewWindow) -> Result<(), String> {
    window.close().map_err(|error| error.to_string())
}

#[tauri::command]
fn window_start_drag(window: WebviewWindow) -> Result<(), String> {
    window.start_dragging().map_err(|error| error.to_string())
}

/// Close every floating image window owned by `owner_label` when that owner window is
/// destroyed, so viewers never outlive the window that spawned them.
fn register_owner_cascade_close(app: &AppHandle, owner_label: &str) {
    let app = app.clone();
    let owner_label = owner_label.to_string();
    if let Some(window) = app.get_webview_window(&owner_label) {
        window.on_window_event(move |event| {
            if !matches!(event, WindowEvent::Destroyed) {
                return;
            }
            let state = app.state::<AppState>();
            let owned: Vec<String> = state
                .image_owners
                .lock()
                .map(|owners| {
                    owners
                        .iter()
                        .filter(|(_, owner)| owner.as_str() == owner_label)
                        .map(|(label, _)| label.clone())
                        .collect()
                })
                .unwrap_or_default();
            for label in owned {
                if let Some(image_window) = app.get_webview_window(&label) {
                    let _ = image_window.close();
                }
            }
        });
    }
}

/// Open a clipboard image in its own borderless floating window, sized to fit the image
/// within the monitor and centered on the double-clicked thumbnail.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn open_image_window(
    app: AppHandle,
    window: WebviewWindow,
    path: String,
    cursor_x: f64,
    cursor_y: f64,
    natural_w: f64,
    natural_h: f64,
) -> Result<(), String> {
    let scale = window
        .scale_factor()
        .map_err(|error| format!("Failed to read scale factor: {error}"))?;
    let owner_position = window
        .outer_position()
        .map_err(|error| format!("Failed to read window position: {error}"))?;
    let owner_x = f64::from(owner_position.x) / scale;
    let owner_y = f64::from(owner_position.y) / scale;
    let cursor_screen_x = owner_x + cursor_x;
    let cursor_screen_y = owner_y + cursor_y;

    let monitor = window
        .current_monitor()
        .map_err(|error| format!("Failed to read current monitor: {error}"))?
        .ok_or_else(|| "No monitor found for window.".to_string())?;
    let monitor_scale = monitor.scale_factor();
    let monitor_x = f64::from(monitor.position().x) / monitor_scale;
    let monitor_y = f64::from(monitor.position().y) / monitor_scale;
    let monitor_w = f64::from(monitor.size().width) / monitor_scale;
    let monitor_h = f64::from(monitor.size().height) / monitor_scale;

    const MIN_WIDTH: f64 = 200.0;
    const MIN_HEIGHT: f64 = 150.0;
    const CURSOR_GAP: f64 = 12.0;
    // "Default" mode fits the image into this comfortable share of the monitor (never upscaling
    // past native) — previously it was capped to the small clipboard window, which made images
    // open tiny.
    const DEFAULT_MONITOR_FRACTION: f64 = 0.72;
    // Keep a little clearance around any opened window so it never sits flush to the monitor
    // edges, even at "True size".
    const SCREEN_MARGIN: f64 = 96.0;

    let opened_image_size = load_settings_inner(&app).opened_image_size;
    let avail_w = (monitor_w - SCREEN_MARGIN).max(MIN_WIDTH);
    let avail_h = (monitor_h - SCREEN_MARGIN).max(MIN_HEIGHT);
    let nat_w = natural_w.max(1.0);
    let nat_h = natural_h.max(1.0);

    let size_scale = if opened_image_size == OPENED_IMAGE_SIZE_DEFAULT {
        // Fit within a large share of the monitor; never upscale past the image's true size.
        (avail_w * DEFAULT_MONITOR_FRACTION / nat_w)
            .min(avail_h * DEFAULT_MONITOR_FRACTION / nat_h)
            .min(1.0)
    } else {
        // Open at the chosen percent of true size, shrunk only as far as needed to fit the monitor.
        let desired = f64::from(opened_image_size) / 100.0;
        desired.min(avail_w / nat_w).min(avail_h / nat_h)
    };

    let width = (nat_w * size_scale).max(MIN_WIDTH.min(avail_w));
    let height = (nat_h * size_scale).max(MIN_HEIGHT.min(avail_h));

    // Place the left edge slightly left of the cursor so the pointer lands just inside the
    // window's draggable edge band — ready to drag immediately. Vertically centered; clamped
    // to the monitor.
    let target_x = (cursor_screen_x - CURSOR_GAP)
        .max(monitor_x)
        .min(monitor_x + monitor_w - width);
    let target_y = (cursor_screen_y - height / 2.0)
        .max(monitor_y)
        .min(monitor_y + monitor_h - height);

    let state = app.state::<AppState>();
    let window_id = state.image_window_counter.fetch_add(1, Ordering::Relaxed) + 1;
    let label = format!("image-{window_id}");

    if let Ok(mut paths) = state.image_paths.lock() {
        paths.insert(label.clone(), path);
    }
    if let Ok(mut owners) = state.image_owners.lock() {
        owners.insert(label.clone(), window.label().to_string());
    }

    let image_window = WebviewWindowBuilder::new(
        &app,
        label.clone(),
        WebviewUrl::App("image-view.html".into()),
    )
    .title("Image")
    .decorations(false)
    .resizable(true)
    .shadow(true)
    .always_on_top(true)
    .background_color(Color(11, 11, 11, 255))
    // Must match the main window's `additionalBrowserArgs` (see tauri.conf.json):
    // WebView2 shares one environment per unique arg set across the whole process, so
    // a second webview with different args fails to initialize and comes up blank.
    .additional_browser_args(
        "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --disable-gpu",
    )
    .build()
    .map_err(|error| format!("Failed to build image window: {error}"))?;

    let corner_preference = Arc::new(AtomicU32::new(0));
    apply_floating_image_corners(&image_window, &corner_preference);
    let _ = set_window_bounds(
        &image_window,
        &WindowState {
            x: target_x.round() as i32,
            y: target_y.round() as i32,
            width: width.round() as u32,
            height: height.round() as u32,
        },
        false,
    );
    let _ = image_window.show();
    let _ = image_window.set_focus();

    let app_for_cleanup = app.clone();
    let label_for_cleanup = label.clone();
    image_window.on_window_event(move |event| match event {
        // Snapping the window changes its size and position, so re-decide rounded vs square
        // corners on every geometry change.
        WindowEvent::Moved(_) | WindowEvent::Resized(_) => {
            if let Some(window) = app_for_cleanup.get_webview_window(&label_for_cleanup) {
                apply_floating_image_corners(&window, &corner_preference);
            }
        }
        WindowEvent::Destroyed => {
            if let Ok(mut paths) = app_for_cleanup.state::<AppState>().image_paths.lock() {
                paths.remove(&label_for_cleanup);
            }
            if let Ok(mut owners) = app_for_cleanup.state::<AppState>().image_owners.lock() {
                owners.remove(&label_for_cleanup);
            }
        }
        _ => {}
    });

    Ok(())
}

#[tauri::command]
fn get_assigned_image_path(window: WebviewWindow, state: tauri::State<AppState>) -> Option<String> {
    state.image_paths.lock().ok()?.get(window.label()).cloned()
}

/// Write a text clipboard item out to a temp `.txt` file and open it in the system's
/// default text editor (the default handler for `.txt`) in its own new window.
#[tauri::command]
fn open_text_in_editor(text: String) -> Result<(), String> {
    if text.is_empty() {
        return Err("No text to open.".to_string());
    }
    // Name the file by a hash of its content so re-opening the same item reuses one temp file
    // instead of accumulating a new one on every double-click.
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    let hash = hasher.finish();
    let dir = std::env::temp_dir().join("clipboard-manager-text");
    fs::create_dir_all(&dir).map_err(|error| format!("Failed to create temp dir: {error}"))?;
    let path = dir.join(format!("clip-{hash:016x}.txt"));
    fs::write(&path, text.as_bytes())
        .map_err(|error| format!("Failed to write temp file: {error}"))?;
    open_path_in_default_app(&path)
}

/// Open a filesystem path with the OS's default handler (for a `.txt` file, that's the
/// user's default text editor), in its own window/process.
#[cfg(windows)]
fn open_path_in_default_app(path: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let verb: Vec<u16> = "open".encode_utf16().chain(std::iter::once(0)).collect();
    let file: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // ShellExecuteW returns an HINSTANCE-shaped handle; any value <= 32 signals failure.
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    if (result as isize) <= 32 {
        return Err(format!(
            "Failed to open default editor (code {})",
            result as isize
        ));
    }
    Ok(())
}

#[cfg(not(windows))]
fn open_path_in_default_app(_path: &Path) -> Result<(), String> {
    Err("Opening the default text editor is only supported on Windows.".to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(AppState::default())
        // Must be registered first. Without it a second launch (a re-clicked shortcut, a
        // startup entry firing while the app is already up) starts a rival process that
        // wipes the running instance's image cache in `setup` below — every preview in the
        // live window goes blank and those items can no longer be copied back — and then
        // sits there as a windowless process, because WebView2 will not hand a second
        // process the same user-data folder. The second launch now hands over and exits
        // before any of that runs.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            // Do NOT build a window here. This callback runs synchronously inside the
            // WM_COPYDATA window procedure while the launching process is blocked in
            // SendMessageW, and building a webview starts a nested message pump inside it
            // that deadlocks both processes. Surfacing the window that already exists is
            // all that is wanted, and it is moved off this thread so the launcher is
            // released immediately.
            let app = app.clone();
            thread::spawn(move || {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.unminimize();
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            });
        }))
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            clean_history_cache(app.handle());
            prune_hidden_history(app.handle());
            start_clipboard_watcher(app.handle().clone());
            if let Some(window) = app.get_webview_window("main") {
                square_window_corners(&window);
                let settings = load_settings_inner(app.handle());
                let _ = apply_window_min_size(&window, settings.displayers_enabled);
                if settings.remember_window_position {
                    if let Some(bounds) = settings.window {
                        let _ =
                            set_window_bounds(&window, &bounds, settings.expand_borderless_edges);
                    }
                }
            }

            // Floating image windows must not outlive the main window that spawned them.
            register_owner_cascade_close(app.handle(), "main");

            // The window starts hidden (visible: false) and the frontend reveals it once it
            // has rendered. If the frontend never gets that far (e.g. a webview that failed to
            // come up at early boot), show it anyway after a short delay so the user is never
            // left staring at a black frame.
            let handle = app.handle().clone();
            thread::spawn(move || {
                thread::sleep(Duration::from_secs(5));
                let state = handle.state::<AppState>();
                if !state.window_shown.load(Ordering::SeqCst) {
                    if let Some(window) = handle.get_webview_window("main") {
                        let _ = window.show();
                    }
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            // This applies only to the main app window, not floating image viewers.
            if window.label() != "main" {
                return;
            }
            // Persist geometry from the backend so the final size/position survives even when
            // the window is closed via the OS (alt-f4 / taskbar), where the frontend's async
            // save during unload can be killed before it completes.
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                if let Some(webview) = window.get_webview_window("main") {
                    let _ = persist_window_state(window.app_handle(), &webview);
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            add_default_image,
            adjust_window_borderless_edges,
            clear_history,
            copy_item_to_clipboard,
            drain_clipboard_items,
            get_assigned_image_path,
            list_default_images,
            load_hidden_history,
            load_settings,
            open_image_window,
            open_text_in_editor,
            paste_default_image,
            remove_default_image,
            save_image_as_default,
            save_hidden_history,
            save_settings,
            save_window_state,
            set_displayers_enabled_window_constraint,
            window_close,
            window_minimize,
            window_show,
            window_start_drag
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app, event| {
            // Backstop against outliving our own window. Nothing here holds a runtime handle
            // past teardown today (both long-lived threads use raw Win32 or non-blocking
            // event emission), but a future one that blocked on the event loop would leave a
            // windowless process pinning its own exe — and the next `tauri build` failing
            // with "os error 5".
            if let tauri::RunEvent::Exit = event {
                std::process::exit(0);
            }
        });
}
