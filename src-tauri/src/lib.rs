use arboard::{Clipboard, ImageData};
use image::{
    codecs::png::{CompressionType, FilterType, PngEncoder},
    ColorType, ImageEncoder, ImageFormat, ImageReader,
};
use serde::{Deserialize, Serialize};
use std::{
    borrow::Cow,
    collections::{hash_map::DefaultHasher, HashMap, VecDeque},
    fs::{self, File},
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Receiver},
        Mutex,
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tauri::{
    utils::config::Color, AppHandle, LogicalPosition, LogicalSize, Manager, Position, Size,
    WebviewUrl, WebviewWindow, WebviewWindowBuilder, WindowEvent,
};

#[cfg(windows)]
use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
#[cfg(windows)]
use windows_sys::Win32::System::DataExchange::GetClipboardSequenceNumber;

const HISTORY_CACHE_DIR: &str = "clipboard-history";
const DEFAULTS_DIR: &str = "default-images";
const SETTINGS_FILE: &str = "settings.json";
const HIDDEN_HISTORY_FILE: &str = "attention-anchor-hidden-history.json";
const IMAGE_EXTS: &[&str] = &["jpg", "jpeg", "png", "gif", "bmp", "webp", "ico"];
const HISTORY_LIMIT: usize = 18;
const BORDERLESS_EDGE_EXPAND: i32 = 1;
const CLIPBOARD_COPY_ATTEMPTS: usize = 6;
const CLIPBOARD_COPY_RETRY_DELAY: Duration = Duration::from_millis(35);

#[cfg(windows)]
const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
#[cfg(windows)]
const DWMWCP_DONOTROUND: u32 = 1;

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
    width: Option<u32>,
    height: Option<u32>,
    fingerprint: String,
    created_at: u128,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ClipboardReadResult {
    sequence: u32,
    item: Option<ClipboardItem>,
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
    while settings.attention_anchors.len() < 6 {
        settings
            .attention_anchors
            .push(defaults.attention_anchors[settings.attention_anchors.len()].clone());
    }
    settings.attention_anchors.truncate(6);

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
            anchor.id = defaults.attention_anchors[index].id.clone();
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

fn image_item_from_rgba(
    app: &AppHandle,
    sequence: u32,
    width: usize,
    height: usize,
    bytes: Vec<u8>,
) -> Result<ClipboardItem, String> {
    image_item_from_rgba_with_timestamp(app, sequence, now_ms(), width, height, bytes)
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
    let file = File::create(&path)
        .map_err(|error| format!("Failed to create clipboard image file: {error}"))?;
    let encoder = PngEncoder::new_with_quality(file, CompressionType::Fast, FilterType::NoFilter);
    encoder
        .write_image(&bytes, width_u32, height_u32, ColorType::Rgba8.into())
        .map_err(|error| format!("Failed to save clipboard image: {error}"))?;

    let fingerprint = format!("image:{width}x{height}:{}", hash_value(&bytes));
    Ok(ClipboardItem {
        id: format!("{sequence}-{created_at}"),
        kind: "image".to_string(),
        text: None,
        file_path: Some(path.to_string_lossy().to_string()),
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
        width: None,
        height: None,
        fingerprint: format!("text:{}:{}", text.len(), hash_value(&text)),
        created_at,
    })
}

fn image_item_from_file(file_path: PathBuf, sequence: u32) -> Result<ClipboardItem, String> {
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
        width: Some(dimensions.0),
        height: Some(dimensions.1),
        fingerprint,
        created_at,
    })
}

fn read_clipboard_image(app: &AppHandle, sequence: u32) -> Option<ClipboardItem> {
    let mut clipboard = Clipboard::new().ok()?;
    let image = clipboard.get_image().ok()?;
    image_item_from_rgba(
        app,
        sequence,
        image.width,
        image.height,
        image.bytes.into_owned(),
    )
    .ok()
}

fn read_clipboard_file_image(sequence: u32) -> Option<ClipboardItem> {
    let files = Clipboard::new().ok()?.get().file_list().ok()?;
    files
        .into_iter()
        .find(|path| path.is_file() && is_image_path(path))
        .and_then(|path| image_item_from_file(path, sequence).ok())
}

fn read_clipboard_text(sequence: u32) -> Option<ClipboardItem> {
    let mut clipboard = Clipboard::new().ok()?;
    let text = clipboard.get_text().ok()?;
    text_item(sequence, now_ms(), text)
}

fn read_clipboard_item(app: &AppHandle, sequence: u32) -> Option<ClipboardItem> {
    read_clipboard_image(app, sequence)
        .or_else(|| read_clipboard_file_image(sequence))
        .or_else(|| read_clipboard_text(sequence))
}

fn raw_file_image_from_paths(paths: Vec<PathBuf>, sequence: u32) -> Option<RawClipboardItem> {
    paths
        .into_iter()
        .find(|path| path.is_file() && is_image_path(path))
        .and_then(|path| image_item_from_file(path, sequence).ok())
        .map(RawClipboardItem::FileImage)
}

fn read_clipboard_raw(sequence: u32) -> Result<Option<RawClipboardItem>, ()> {
    let mut clipboard = Clipboard::new().map_err(|_| ())?;
    let created_at = now_ms();

    if let Ok(image) = clipboard.get_image() {
        return Ok(Some(RawClipboardItem::Image {
            sequence,
            created_at,
            width: image.width,
            height: image.height,
            bytes: image.bytes.into_owned(),
        }));
    }

    if let Ok(paths) = clipboard.get().file_list() {
        if let Some(item) = raw_file_image_from_paths(paths, sequence) {
            return Ok(Some(item));
        }
    }

    if let Ok(text) = clipboard.get_text() {
        if let Some(item) = text_item(sequence, created_at, text) {
            return Ok(Some(RawClipboardItem::Text {
                sequence,
                created_at,
                text: item.text.unwrap_or_default(),
            }));
        }
    }

    Ok(None)
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

    thread::spawn(move || {
        let mut last_sequence = 0u32;
        loop {
            let sequence = clipboard_sequence();
            if sequence != 0 && sequence != last_sequence {
                match read_clipboard_raw(sequence) {
                    Ok(item) => {
                        if let Some(item) = item {
                            let _ = sender.send(item);
                        }
                        last_sequence = sequence;
                    }
                    Err(()) => {
                        thread::sleep(std::time::Duration::from_millis(20));
                    }
                }
            }
            thread::sleep(std::time::Duration::from_millis(45));
        }
    });
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

#[cfg(windows)]
fn square_window_corners(window: &WebviewWindow) {
    let Ok(hwnd) = window.hwnd() else {
        return;
    };
    let preference = DWMWCP_DONOTROUND;
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd.0 as _,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            (&preference as *const u32).cast(),
            std::mem::size_of_val(&preference) as u32,
        );
    }
}

#[cfg(not(windows))]
fn square_window_corners(_window: &WebviewWindow) {}

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

#[tauri::command]
fn load_hidden_history(app: AppHandle) -> Result<Vec<ClipboardItem>, String> {
    let path = hidden_history_path(&app)?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let data = fs::read_to_string(path)
        .map_err(|error| format!("Failed to read hidden history: {error}"))?;
    serde_json::from_str::<Vec<ClipboardItem>>(&data)
        .map_err(|error| format!("Failed to parse hidden history: {error}"))
}

#[tauri::command]
fn read_clipboard(
    app: AppHandle,
    last_sequence: Option<u32>,
) -> Result<ClipboardReadResult, String> {
    let sequence = clipboard_sequence();
    if last_sequence == Some(sequence) {
        return Ok(ClipboardReadResult {
            sequence,
            item: None,
        });
    }

    Ok(ClipboardReadResult {
        sequence,
        item: read_clipboard_item(&app, sequence),
    })
}

#[tauri::command]
fn copy_item_to_clipboard(
    kind: String,
    text: Option<String>,
    file_path: Option<String>,
) -> Result<(), String> {
    match kind.as_str() {
        "text" => {
            let text = text.ok_or_else(|| "No text content was supplied.".to_string())?;
            copy_text_to_clipboard(text)
        }
        "image" => {
            let file_path = file_path.ok_or_else(|| "No image path was supplied.".to_string())?;
            copy_image_to_clipboard(Path::new(&file_path))
        }
        _ => Err("Unsupported clipboard item type.".to_string()),
    }
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
    let owner_size = window
        .inner_size()
        .map_err(|error| format!("Failed to read window size: {error}"))?;
    let owner_x = f64::from(owner_position.x) / scale;
    let owner_y = f64::from(owner_position.y) / scale;
    let owner_w = f64::from(owner_size.width) / scale;
    let owner_h = f64::from(owner_size.height) / scale;
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

    // Never exceed the main clipboard window's size; also stay within the monitor. This makes
    // images open smaller whenever the clipboard window is smaller.
    let max_w = owner_w.min(monitor_w);
    let max_h = owner_h.min(monitor_h);
    let fit_scale = (max_w / natural_w.max(1.0))
        .min(max_h / natural_h.max(1.0))
        .min(1.0);
    let width = (natural_w * fit_scale).max(MIN_WIDTH.min(max_w));
    let height = (natural_h * fit_scale).max(MIN_HEIGHT.min(max_h));

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

    square_window_corners(&image_window);
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
    image_window.on_window_event(move |event| {
        if matches!(event, WindowEvent::Destroyed) {
            if let Ok(mut paths) = app_for_cleanup.state::<AppState>().image_paths.lock() {
                paths.remove(&label_for_cleanup);
            }
            if let Ok(mut owners) = app_for_cleanup.state::<AppState>().image_owners.lock() {
                owners.remove(&label_for_cleanup);
            }
        }
    });

    Ok(())
}

#[tauri::command]
fn get_assigned_image_path(window: WebviewWindow, state: tauri::State<AppState>) -> Option<String> {
    state.image_paths.lock().ok()?.get(window.label()).cloned()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(AppState::default())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            clean_history_cache(app.handle());
            start_clipboard_watcher(app.handle().clone());
            if let Some(window) = app.get_webview_window("main") {
                square_window_corners(&window);
                let settings = load_settings_inner(app.handle());
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
            paste_default_image,
            read_clipboard,
            remove_default_image,
            save_image_as_default,
            save_hidden_history,
            save_settings,
            save_window_state,
            window_close,
            window_minimize,
            window_show,
            window_start_drag
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
