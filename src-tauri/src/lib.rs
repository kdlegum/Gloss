use sqlx::{sqlite::SqlitePoolOptions, Row, SqlitePool};
use tauri::Manager;
use tauri_plugin_dialog::DialogExt;

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
}

#[derive(serde::Serialize)]
struct Textbook {
    id: i64,
    title: String,
    file_path: String,
}

#[tauri::command]
async fn import_pdf(
    app: tauri::AppHandle,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<Textbook, String> {
    // Open file picker
    let picked = app
        .dialog()
        .file()
        .add_filter("PDF", &["pdf"])
        .blocking_pick_file();

    let src_path = match picked {
        Some(p) => p.into_path().map_err(|e| e.to_string())?,
        None => return Err("cancelled".into()),
    };

    // Derive destination inside the app data directory
    let file_name = src_path
        .file_name()
        .ok_or("invalid file name")?
        .to_string_lossy()
        .to_string();

    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let pdfs_dir = data_dir.join("pdfs");
    std::fs::create_dir_all(&pdfs_dir).map_err(|e| e.to_string())?;

    // Avoid overwriting an existing file by appending a counter if needed
    let mut dest_path = pdfs_dir.join(&file_name);
    if dest_path.exists() {
        let stem = src_path
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

    std::fs::copy(&src_path, &dest_path).map_err(|e| e.to_string())?;

    // Relative path stored in DB (relative to app data dir)
    let relative_path = format!(
        "pdfs/{}",
        dest_path.file_name().unwrap().to_string_lossy()
    );

    let title = src_path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    let row = sqlx::query("INSERT INTO textbooks (title, file_path) VALUES (?, ?) RETURNING id")
        .bind(&title)
        .bind(&relative_path)
        .fetch_one(pool.inner())
        .await
        .map_err(|e| e.to_string())?;

    let id: i64 = row.get("id");

    Ok(Textbook {
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
async fn list_textbooks(pool: tauri::State<'_, SqlitePool>) -> Result<Vec<Textbook>, String> {
    let rows =
        sqlx::query("SELECT id, title, file_path FROM textbooks ORDER BY id DESC")
            .fetch_all(pool.inner())
            .await
            .map_err(|e| e.to_string())?;

    Ok(rows
        .into_iter()
        .map(|r| Textbook {
            id: r.get("id"),
            title: r.get("title"),
            file_path: r.get("file_path"),
        })
        .collect())
}

#[tauri::command]
async fn get_or_create_page(
    textbook_id: i64,
    page_number: i64,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<i64, String> {
    sqlx::query(
        "INSERT OR IGNORE INTO pages (textbook_id, page_number) VALUES (?, ?)",
    )
    .bind(textbook_id)
    .bind(page_number)
    .execute(pool.inner())
    .await
    .map_err(|e| e.to_string())?;

    let row = sqlx::query("SELECT id FROM pages WHERE textbook_id = ? AND page_number = ?")
        .bind(textbook_id)
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
        "INSERT INTO strokes (page_id, data, colour, min_x, min_y, max_x, max_y) \
         VALUES (?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(page_id)
    .bind(&data)
    .bind(&stroke.colour)
    .bind(min_x)
    .bind(min_y)
    .bind(max_x)
    .bind(max_y)
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
    let rows = sqlx::query("SELECT id, colour, data, min_x, min_y, max_x, max_y FROM strokes WHERE page_id = ? ORDER BY id")
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
            })
        })
        .collect()
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
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let pool = tauri::async_runtime::block_on(init_db(app))
                .expect("failed to initialise database");
            app.manage(pool);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![import_pdf, list_textbooks, get_pdf_path, get_or_create_page, save_stroke, load_strokes, delete_stroke])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
