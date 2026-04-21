use percent_encoding::percent_decode_str;
use pdfium_render::prelude::*;
use sqlx::{sqlite::SqlitePoolOptions, Row, SqlitePool};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tauri::Manager;
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_fs::{FilePath, FsExt};

// ── Pdfium worker ────────────────────────────────────────────────────────────
// pdfium_render's Pdfium is !Send, so it must live on a single dedicated thread.
// All PDF work is dispatched to that thread via a channel.

type PdfJob = Box<dyn FnOnce(&Pdfium) + Send + 'static>;

#[derive(Clone)]
struct PdfiumWorker(std::sync::mpsc::SyncSender<PdfJob>);

impl PdfiumWorker {
    /// Spawn the worker thread and bind pdfium.
    fn spawn(lib_path: std::path::PathBuf) -> Result<Self, String> {
        let (tx, rx) = std::sync::mpsc::sync_channel::<PdfJob>(64);
        std::thread::Builder::new()
            .name("pdfium-worker".into())
            .spawn(move || {
                let pdfium = Pdfium::new(
                    Pdfium::bind_to_library(&lib_path)
                        .expect("failed to bind pdfium library"),
                );
                for job in rx {
                    job(&pdfium);
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self(tx))
    }

    /// Spawn the worker thread, binding pdfium from the system loader (Android).
    fn spawn_system() -> Result<Self, String> {
        let (tx, rx) = std::sync::mpsc::sync_channel::<PdfJob>(64);
        std::thread::Builder::new()
            .name("pdfium-worker".into())
            .spawn(move || {
                let pdfium = Pdfium::new(
                    Pdfium::bind_to_system_library()
                        .expect("failed to bind pdfium system library"),
                );
                for job in rx {
                    job(&pdfium);
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self(tx))
    }

    /// Run a closure on the pdfium thread and await its result.
    async fn run<F, T>(&self, f: F) -> Result<T, String>
    where
        F: FnOnce(&Pdfium) -> Result<T, String> + Send + 'static,
        T: Send + 'static,
    {
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        let job: PdfJob = Box::new(move |pdfium| {
            let _ = done_tx.send(f(pdfium));
        });
        self.0.send(job).map_err(|_| "pdfium worker closed".to_string())?;
        done_rx.await.map_err(|_| "pdfium worker dropped result".to_string())?
    }
}

// ── Rendered page ────────────────────────────────────────────────────────────
// Raw RGBA bytes + dimensions. Sent directly to the frontend so it can build
// an ImageData without any PNG encode/decode round-trip.
#[derive(serde::Serialize, Clone)]
struct RenderedPage {
    data: Vec<u8>,
    width: u32,
    height: u32,
}

// ── PDF render cache ─────────────────────────────────────────────────────────
// Keyed by (relative_path, page_number, scale * 1000 as u32).
// Stores raw RGBA renders so we never re-rasterize the same page twice.
const PDF_CACHE_MAX: usize = 20;

struct PdfCache {
    order: Vec<(String, usize, u32)>,
    data: HashMap<(String, usize, u32), Arc<RenderedPage>>,
}

impl PdfCache {
    fn new() -> Self {
        Self {
            order: Vec::new(),
            data: HashMap::new(),
        }
    }

    fn get(&mut self, key: &(String, usize, u32)) -> Option<Arc<RenderedPage>> {
        if let Some(val) = self.data.get(key) {
            // Move to front (most recently used)
            self.order.retain(|k| k != key);
            self.order.insert(0, key.clone());
            return Some(Arc::clone(val));
        }
        None
    }

    fn put(&mut self, key: (String, usize, u32), value: RenderedPage) -> Arc<RenderedPage> {
        if self.data.contains_key(&key) {
            self.order.retain(|k| k != &key);
        } else if self.order.len() >= PDF_CACHE_MAX {
            if let Some(evict) = self.order.pop() {
                self.data.remove(&evict);
            }
        }
        self.order.insert(0, key.clone());
        let arc = Arc::new(value);
        self.data.insert(key, Arc::clone(&arc));
        arc
    }
}

struct AppState {
    pdf_cache: Mutex<PdfCache>,
    pdfium: PdfiumWorker,
}

#[derive(serde::Deserialize, serde::Serialize, bincode::Encode, bincode::Decode, Clone, Copy)]
struct Point {
    x: f32,
    y: f32,
}

/// What the frontend sends for a single stroke.
#[derive(serde::Deserialize)]
struct StrokeInput {
    colour: String,
    points: Vec<Point>,
    chunk_id: Option<i64>,
}

#[derive(serde::Serialize)]
struct StrokeOutput {
    id: i64,
    colour: String,
    points: Vec<Point>,
    min_x: f32,
    min_y: f32,
    max_x: f32,
    max_y: f32,
    chunk_id: Option<i64>,
}

#[derive(serde::Serialize)]
struct SourceDocument {
    id: i64,
    title: String,
    file_path: String,
}

#[tauri::command]
async fn import_pdf(
    app: tauri::AppHandle,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<SourceDocument, String> {
    // Open file picker — returns FilePath::Path on desktop, FilePath::Url on Android
    let picked = app
        .dialog()
        .file()
        .add_filter("PDF", &["pdf"])
        .blocking_pick_file();

    let file_path = match picked {
        Some(p) => p,
        None => return Err("cancelled".into()),
    };

    // Extract filename. On Android the content URI's last path segment is percent-encoded
    // and may contain a sub-path like "primary:Download/foo.pdf" — take just the tail.
    let file_name: String = match &file_path {
        FilePath::Path(p) => p
            .file_name()
            .ok_or("invalid file name")?
            .to_string_lossy()
            .to_string(),
        FilePath::Url(u) => {
            let raw = u
                .path_segments()
                .and_then(|mut s| s.next_back().map(str::to_owned))
                .unwrap_or_else(|| "import.pdf".into());
            // Percent-decode and take the tail after any "/" within the segment
            // e.g. "primary%3ADownload%2Ffoo.pdf" -> "foo.pdf"
            let decoded = percent_decode_str(&raw).decode_utf8_lossy().to_string();
            decoded
                .rsplit('/')
                .next()
                .unwrap_or("import.pdf")
                .to_string()
        }
    };

    let file_name = if file_name.to_lowercase().ends_with(".pdf") {
        file_name
    } else {
        format!("{}.pdf", file_name)
    };

    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let pdfs_dir = data_dir.join("pdfs");
    std::fs::create_dir_all(&pdfs_dir).map_err(|e| e.to_string())?;

    // Avoid overwriting an existing file by appending a counter if needed
    let mut dest_path = pdfs_dir.join(&file_name);
    if dest_path.exists() {
        let stem = std::path::Path::new(&file_name)
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let mut counter: u32 = 1;
        loop {
            dest_path = pdfs_dir.join(format!("{}_{}.pdf", stem, counter));
            if !dest_path.exists() {
                break;
            }
            counter += 1;
        }
    }

    // Read via tauri-plugin-fs — handles content:// URIs on Android via JNI,
    // falls back to plain std::fs on desktop.
    let bytes = app.fs().read(file_path).map_err(|e| e.to_string())?;
    std::fs::write(&dest_path, &bytes).map_err(|e| e.to_string())?;

    // Relative path stored in DB (relative to app data dir)
    let relative_path = format!(
        "pdfs/{}",
        dest_path.file_name().unwrap().to_string_lossy()
    );

    let title = std::path::Path::new(&file_name)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    let row = sqlx::query("INSERT INTO source_documents (title, file_path) VALUES (?, ?) RETURNING id")
        .bind(&title)
        .bind(&relative_path)
        .fetch_one(pool.inner())
        .await
        .map_err(|e| e.to_string())?;

    let id: i64 = row.get("id");

    Ok(SourceDocument {
        id,
        title,
        file_path: relative_path,
    })
}

#[tauri::command]
async fn get_pdf_path(
    app: tauri::AppHandle,
    relative_path: String,
) -> Result<String, String> {
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let abs = data_dir.join(&relative_path);
    Ok(abs.to_string_lossy().to_string())
}

#[tauri::command]
async fn list_textbooks(pool: tauri::State<'_, SqlitePool>) -> Result<Vec<SourceDocument>, String> {
    let rows =
        sqlx::query("SELECT id, title, file_path FROM source_documents ORDER BY id DESC")
            .fetch_all(pool.inner())
            .await
            .map_err(|e| e.to_string())?;

    Ok(rows
        .into_iter()
        .map(|r| SourceDocument {
            id: r.get("id"),
            title: r.get("title"),
            file_path: r.get("file_path"),
        })
        .collect())
}

#[tauri::command]
async fn get_or_create_page(
    source_document_id: i64,
    page_number: i64,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<i64, String> {
    sqlx::query(
        "INSERT OR IGNORE INTO pages (source_document_id, page_number) VALUES (?, ?)",
    )
    .bind(source_document_id)
    .bind(page_number)
    .execute(pool.inner())
    .await
    .map_err(|e| e.to_string())?;

    let row = sqlx::query("SELECT id FROM pages WHERE source_document_id = ? AND page_number = ?")
        .bind(source_document_id)
        .bind(page_number)
        .fetch_one(pool.inner())
        .await
        .map_err(|e| e.to_string())?;

    Ok(row.get("id"))
}

#[tauri::command]
async fn save_stroke(
    page_id: i64,
    stroke: StrokeInput,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<i64, String> {
    if stroke.points.is_empty() {
        return Err("stroke has no points".into());
    }

    // Bounding box
    let mut min_x = stroke.points[0].x;
    let mut min_y = stroke.points[0].y;
    let mut max_x = min_x;
    let mut max_y = min_y;
    for p in &stroke.points[1..] {
        if p.x < min_x { min_x = p.x; }
        if p.y < min_y { min_y = p.y; }
        if p.x > max_x { max_x = p.x; }
        if p.y > max_y { max_y = p.y; }
    }

    // Serialise points with bincode
    let data: Vec<u8> = bincode::encode_to_vec(&stroke.points, bincode::config::standard())
        .map_err(|e| e.to_string())?;

    let row = sqlx::query(
        "INSERT INTO strokes (page_id, data, colour, min_x, min_y, max_x, max_y, chunk_id) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(page_id)
    .bind(&data)
    .bind(&stroke.colour)
    .bind(min_x)
    .bind(min_y)
    .bind(max_x)
    .bind(max_y)
    .bind(stroke.chunk_id)
    .fetch_one(pool.inner())
    .await
    .map_err(|e| e.to_string())?;

    Ok(row.get("id"))
}

#[tauri::command]
async fn delete_stroke(
    stroke_id: i64,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<(), String> {
    sqlx::query("DELETE FROM strokes WHERE id = ?")
        .bind(stroke_id)
        .execute(pool.inner())
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn load_strokes(
    page_id: i64,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<Vec<StrokeOutput>, String> {
    let rows = sqlx::query("SELECT id, colour, data, min_x, min_y, max_x, max_y, chunk_id FROM strokes WHERE page_id = ? ORDER BY id")
        .bind(page_id)
        .fetch_all(pool.inner())
        .await
        .map_err(|e| e.to_string())?;

    rows.into_iter()
        .map(|r| {
            let data: Vec<u8> = r.get("data");
            let (points, _) =
                bincode::decode_from_slice::<Vec<Point>, _>(&data, bincode::config::standard())
                    .map_err(|e| e.to_string())?;
            Ok(StrokeOutput {
                id: r.get("id"),
                colour: r.get("colour"),
                points,
                min_x: r.get("min_x"),
                min_y: r.get("min_y"),
                max_x: r.get("max_x"),
                max_y: r.get("max_y"),
                chunk_id: r.get("chunk_id"),
            })
        })
        .collect()
}

/// Return the number of pages in the PDF at the given relative path.
#[tauri::command]
async fn get_page_count(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    relative_path: String,
) -> Result<usize, String> {
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let abs_path = data_dir.join(&relative_path);

    state.pdfium.run(move |pdfium| {
        let doc = pdfium.load_pdf_from_file(&abs_path, None).map_err(|e| e.to_string())?;
        Ok(doc.pages().len() as usize)
    }).await
}

/// Render a single PDF page and return raw RGBA bytes with dimensions.
/// `scale` is CSS pixels per PDF user-unit (point). Typical value: 1.0–2.0.
/// Results are cached by (path, page, scale) so subsequent calls are O(1).
///
/// Returns a binary response: [width: u32 LE][height: u32 LE][RGBA data...].
/// Using `tauri::ipc::Response` sends raw bytes over IPC instead of JSON-encoding
/// millions of pixel values, which would otherwise take seconds.
#[tauri::command]
async fn render_pdf_page(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    relative_path: String,
    page_number: usize,
    scale: f32,
) -> Result<tauri::ipc::Response, String> {
    let scale_key = (scale * 1000.0).round() as u32;
    let cache_key = (relative_path.clone(), page_number, scale_key);

    // Check cache first (lock briefly, then release)
    {
        let mut cache = state.pdf_cache.lock().map_err(|e| e.to_string())?;
        if let Some(page) = cache.get(&cache_key) {
            return Ok(rendered_page_to_response(&page));
        }
    }

    // Resolve absolute path
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let abs_path = data_dir.join(&relative_path);

    // Render on the dedicated pdfium worker thread
    let rendered = state.pdfium.run(move |pdfium| {
        let doc = pdfium
            .load_pdf_from_file(&abs_path, None)
            .map_err(|e| e.to_string())?;

        let page = doc
            .pages()
            .get(page_number as i32)
            .map_err(|e| e.to_string())?;

        // pdfium page dimensions are in PDF user-units (points). scale converts to CSS px.
        let width_px = (page.width().value * scale).round() as i32;
        let height_px = (page.height().value * scale).round() as i32;

        let bitmap = page
            .render_with_config(
                &PdfRenderConfig::new()
                    .set_target_width(width_px)
                    .set_target_height(height_px),
            )
            .map_err(|e| e.to_string())?;

        // Return raw RGBA bytes — no PNG encode/decode round-trip needed.
        Ok(RenderedPage {
            width: bitmap.width() as u32,
            height: bitmap.height() as u32,
            data: bitmap.as_rgba_bytes(),
        })
    }).await?;

    // Store in cache
    let cached = {
        let mut cache = state.pdf_cache.lock().map_err(|e| e.to_string())?;
        cache.put(cache_key, rendered)
    };
    Ok(rendered_page_to_response(&cached))
}

/// Pack a RenderedPage into a binary IPC response: width (4B LE) + height (4B LE) + RGBA data.
fn rendered_page_to_response(page: &RenderedPage) -> tauri::ipc::Response {
    let mut buf = Vec::with_capacity(8 + page.data.len());
    buf.extend_from_slice(&page.width.to_le_bytes());
    buf.extend_from_slice(&page.height.to_le_bytes());
    buf.extend_from_slice(&page.data);
    tauri::ipc::Response::new(buf)
}

async fn init_db(app: &tauri::App) -> Result<SqlitePool, Box<dyn std::error::Error>> {
    let data_dir = app.path().app_data_dir()?;
    std::fs::create_dir_all(&data_dir)?;

    let db_path = data_dir.join("gloss.db");
    let db_url = format!("sqlite://{}?mode=rwc", db_path.display());

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await?;

    sqlx::migrate!("./migrations").run(&pool).await?;

    Ok(pool)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let pool = tauri::async_runtime::block_on(init_db(app))
                .expect("failed to initialise database");
            app.manage(pool);

            let pdfium = if cfg!(target_os = "android") {
                PdfiumWorker::spawn_system()
                    .expect("failed to start pdfium worker")
            } else {
                let exe_dir = std::env::current_exe()
                    .expect("can't find exe")
                    .parent()
                    .expect("exe has no parent dir")
                    .to_path_buf();
                let lib_name = if cfg!(target_os = "windows") { "pdfium.dll" }
                    else if cfg!(target_os = "macos") { "libpdfium.dylib" }
                    else { "libpdfium.so" };
                PdfiumWorker::spawn(exe_dir.join(lib_name))
                    .expect("failed to start pdfium worker")
            };

            app.manage(AppState {
                pdf_cache: Mutex::new(PdfCache::new()),
                pdfium,
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            import_pdf,
            list_textbooks,
            get_pdf_path,
            get_or_create_page,
            save_stroke,
            load_strokes,
            delete_stroke,
            render_pdf_page,
            get_page_count,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
