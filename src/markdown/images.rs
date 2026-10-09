use ratatui::{
    layout::Size,
    style::{Color, Style},
    text::{Line, Span},
};
use ratatui_image::{picker::Picker, sliced::SlicedProtocol, Resize};
use std::{
    collections::HashMap,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::Duration,
};

const REMOTE_IMAGE_MAX_BYTES: u64 = 20 * 1024 * 1024;
const REMOTE_IMAGE_TIMEOUT: Duration = Duration::from_secs(10);
const REMOTE_IMAGE_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// Off by default so outputs that can't draw images (`--inline`) and tests never hit the
/// network; the TUI turns it on at startup.
static REMOTE_IMAGES_ENABLED: AtomicBool = AtomicBool::new(false);

/// Downloaded bytes per URL, failures included (`None`), so re-parses on resize, reload or
/// theme preview don't re-download or re-wait on an unreachable host.
type RemoteCache = Mutex<HashMap<String, Option<Arc<Vec<u8>>>>>;
static REMOTE_CACHE: OnceLock<RemoteCache> = OnceLock::new();

pub(crate) fn enable_remote_images() {
    REMOTE_IMAGES_ENABLED.store(true, Ordering::Relaxed);
}

/// A decoded, terminal-ready image placed at `rendered_start` in the parsed line buffer.
pub(crate) struct ImageEntry {
    pub(crate) rendered_start: usize,
    pub(crate) slice: SlicedProtocol,
    /// Text stand-in for outputs that can't draw graphics (e.g. `--inline`).
    pub(crate) placeholder: Line<'static>,
}

pub(crate) fn image_placeholder(alt: &str, color: Color) -> Line<'static> {
    Line::from(Span::styled(
        format!("[img: {alt}]"),
        Style::default().fg(color),
    ))
}

fn is_http_url(dest: &str) -> bool {
    dest.starts_with("http://") || dest.starts_with("https://")
}

/// Blocking download of an image's raw bytes, capped at `REMOTE_IMAGE_MAX_BYTES`.
pub(crate) fn fetch_remote_image(url: &str) -> Option<Vec<u8>> {
    let client = reqwest::blocking::Client::builder()
        .timeout(REMOTE_IMAGE_TIMEOUT)
        .connect_timeout(REMOTE_IMAGE_CONNECT_TIMEOUT)
        .user_agent(concat!("leaf/", env!("CARGO_PKG_VERSION")))
        .build()
        .ok()?;
    let resp = client.get(url).send().ok()?.error_for_status().ok()?;
    if resp
        .content_length()
        .is_some_and(|len| len > REMOTE_IMAGE_MAX_BYTES)
    {
        return None;
    }
    let mut bytes = Vec::new();
    resp.take(REMOTE_IMAGE_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= REMOTE_IMAGE_MAX_BYTES).then_some(bytes)
}

fn cached_remote_image(url: &str) -> Option<Arc<Vec<u8>>> {
    let cache = REMOTE_CACHE.get_or_init(Default::default);
    if let Some(hit) = cache.lock().ok()?.get(url) {
        return hit.clone();
    }
    // Fetch without holding the lock; a duplicate concurrent fetch is harmless.
    let fetched = fetch_remote_image(url).map(Arc::new);
    cache.lock().ok()?.insert(url.to_string(), fetched.clone());
    fetched
}

fn decode_image(dest: &str, base_path: Option<&Path>) -> Option<image::DynamicImage> {
    if is_http_url(dest) {
        if !REMOTE_IMAGES_ENABLED.load(Ordering::Relaxed) {
            return None;
        }
        let bytes = cached_remote_image(dest)?;
        return image::load_from_memory(&bytes).ok();
    }
    if dest.starts_with("data:") {
        return None;
    }
    let path = resolve_image_path(dest, base_path);
    image::ImageReader::open(&path).ok()?.decode().ok()
}

fn resolve_image_path(dest: &str, base_path: Option<&Path>) -> PathBuf {
    let path = Path::new(dest);
    if path.is_absolute() {
        return path.to_path_buf();
    }
    match base_path {
        Some(base) => base.join(path),
        None => path.to_path_buf(),
    }
}

/// Decodes a local or `http(s)` image and slices it to fit within `render_width` columns,
/// preserving its aspect ratio via the terminal's font-cell size. Terminals without a
/// graphics protocol get halfblock art. Returns `None` for `data:` URIs, remote URLs while
/// remote fetching is disabled, or images that can't be read/decoded, in which case callers
/// should fall back to a text placeholder.
pub(crate) fn load_image(
    dest: &str,
    base_path: Option<&Path>,
    render_width: usize,
    picker: &Picker,
) -> Option<(SlicedProtocol, usize)> {
    let dyn_img = decode_image(dest, base_path)?;

    let font_size = picker.font_size();
    // Unbounded height: images are allowed to run tall and simply scroll, like code blocks.
    let bounds = Size::new(render_width.max(1) as u16, u16::MAX);
    let size = Resize::Fit(None).size_for(&dyn_img, font_size, bounds);
    let slice = SlicedProtocol::new_with_resize(picker, dyn_img, size, Resize::Fit(None)).ok()?;
    let height = (slice.size().height as usize).max(1);
    Some((slice, height))
}

/// Collapses each image's reserved blank rows into its single-line placeholder.
/// `images` must be sorted by `rendered_start`, as the parser produces them.
pub(crate) fn replace_images_with_placeholders(
    lines: Vec<Line<'static>>,
    images: &[ImageEntry],
) -> Vec<Line<'static>> {
    let mut out = Vec::with_capacity(lines.len());
    let mut images = images.iter().peekable();
    let mut skip_until = 0;
    for (idx, line) in lines.into_iter().enumerate() {
        if idx < skip_until {
            continue;
        }
        if let Some(entry) = images.next_if(|e| e.rendered_start == idx) {
            out.push(entry.placeholder.clone());
            skip_until = idx + entry.slice.size().height as usize;
            continue;
        }
        out.push(line);
    }
    out
}
