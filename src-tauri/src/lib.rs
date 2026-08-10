use lofty::{
    file::{AudioFile, TaggedFileExt},
    probe::Probe,
    tag::{Accessor, ItemKey, Tag},
};
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::{
    collections::HashMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Manager, State};
use walkdir::WalkDir;

struct Db(Mutex<Connection>);

struct ScanManager {
    status: Arc<Mutex<ScanStatus>>,
    cancel: Arc<AtomicBool>,
    db_path: PathBuf,
}

#[derive(Clone, Serialize)]
struct ScanStatus {
    running: bool,
    phase: String,
    current: usize,
    total: usize,
    path: String,
    message: String,
}

impl Default for ScanStatus {
    fn default() -> Self {
        Self {
            running: false,
            phase: "idle".into(),
            current: 0,
            total: 0,
            path: String::new(),
            message: String::new(),
        }
    }
}

#[derive(Serialize)]
struct Library {
    id: i64,
    path: String,
    name: String,
    exists: bool,
}

#[derive(Default, Clone)]
struct UcsMeta {
    cat_id: String,
    category: String,
    sub_category: String,
    user_category: String,
    vendor_category: String,
    fx_name: String,
    creator_id: String,
    source_id: String,
    user_data: String,
    ucs_library: String,
}

#[derive(Serialize)]
struct AudioItem {
    id: i64,
    path: String,
    name: String,
    ext: String,
    library: String,
    description: String,
    duration: f64,
    rating: f64,
    artist: String,
    album: String,
    genre: String,
    year: String,
    tags: String,
    scanned_at: i64,
    favorite: bool,
    cat_id: String,
    category: String,
    sub_category: String,
    user_category: String,
    vendor_category: String,
    fx_name: String,
    creator_id: String,
    source_id: String,
    user_data: String,
    ucs_library: String,
    path_exists: bool,
    library_exists: bool,
}

const AUDIO_EXTS: &[&str] = &[
    "wav", "wave", "mp3", "flac", "ogg", "oga", "opus", "m4a", "mp4", "aac", "aiff", "aif", "aifc",
    "wma", "ape", "wv", "caf", "ac3", "amr", "mid", "midi",
];
const UCS_COLUMNS: &[(&str, &str)] = &[
    ("cat_id", "TEXT NOT NULL DEFAULT ''"),
    ("category", "TEXT NOT NULL DEFAULT ''"),
    ("sub_category", "TEXT NOT NULL DEFAULT ''"),
    ("user_category", "TEXT NOT NULL DEFAULT ''"),
    ("vendor_category", "TEXT NOT NULL DEFAULT ''"),
    ("fx_name", "TEXT NOT NULL DEFAULT ''"),
    ("creator_id", "TEXT NOT NULL DEFAULT ''"),
    ("source_id", "TEXT NOT NULL DEFAULT ''"),
    ("user_data", "TEXT NOT NULL DEFAULT ''"),
    ("ucs_library", "TEXT NOT NULL DEFAULT ''"),
];

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
fn text_path(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}
fn is_installed_dir(dir: &Path) -> bool {
    if dir.join("uninstall.exe").exists() {
        return true;
    }
    let normalized = dir
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    if normalized.contains("/program files/")
        || normalized.contains("/program files (x86)/")
        || normalized.contains("/applications/lings.app/")
    {
        return true;
    }
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        let local = local.replace('\\', "/").to_ascii_lowercase();
        if normalized.starts_with(&local)
            && dir
                .file_name()
                .map(|x| x.to_string_lossy().eq_ignore_ascii_case("lings"))
                .unwrap_or(false)
        {
            return true;
        }
    }
    false
}
fn db_path(_app: &AppHandle) -> PathBuf {
    let exe = std::env::current_exe().ok();
    let dir = exe
        .as_deref()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let folder = if is_installed_dir(&dir) {
        "database"
    } else {
        "Lings_db"
    };
    dir.join(folder).join("lings.db")
}
fn legacy_db_path(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_data_dir().ok().map(|p| p.join("lings.db"))
}

fn init_db(app: &AppHandle) -> Result<Connection, String> {
    let path = db_path(app);
    if let Some(p) = path.parent() {
        fs::create_dir_all(p)
            .map_err(|e| format!("Cannot create database folder {}: {e}", p.display()))?
    }
    if !path.exists() {
        if let Some(old) = legacy_db_path(app) {
            if old.exists() && old != path {
                fs::copy(old, &path).map_err(|e| {
                    format!(
                        "Cannot migrate existing database to {}: {e}",
                        path.display()
                    )
                })?;
            }
        }
    }
    let c = Connection::open(path).map_err(|e| e.to_string())?;
    c.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;
      CREATE TABLE IF NOT EXISTS libraries(id INTEGER PRIMARY KEY,path TEXT NOT NULL UNIQUE,name TEXT NOT NULL,added_at INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS audio(id INTEGER PRIMARY KEY,path TEXT NOT NULL UNIQUE,name TEXT NOT NULL,ext TEXT NOT NULL,library_id INTEGER NOT NULL,description TEXT NOT NULL DEFAULT '',duration REAL NOT NULL DEFAULT 0,rating REAL NOT NULL DEFAULT 0,artist TEXT NOT NULL DEFAULT '',album TEXT NOT NULL DEFAULT '',genre TEXT NOT NULL DEFAULT '',year TEXT NOT NULL DEFAULT '',tags TEXT NOT NULL DEFAULT '',scanned_at INTEGER NOT NULL,playlist INTEGER NOT NULL DEFAULT 0,favorite INTEGER NOT NULL DEFAULT 0,FOREIGN KEY(library_id) REFERENCES libraries(id) ON DELETE CASCADE);
      CREATE INDEX IF NOT EXISTS idx_audio_name ON audio(name); CREATE INDEX IF NOT EXISTS idx_audio_scan ON audio(scanned_at);").map_err(|e|e.to_string())?;
    let existing: Vec<String> = {
        let mut s = c
            .prepare("PRAGMA table_info(audio)")
            .map_err(|e| e.to_string())?;
        let rows = s
            .query_map([], |r| r.get(1))
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .collect();
        rows
    };
    for (name, ty) in UCS_COLUMNS {
        if !existing.iter().any(|x| x == name) {
            c.execute(&format!("ALTER TABLE audio ADD COLUMN {name} {ty}"), [])
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(c)
}

fn read_generic(path: &Path) -> (f64, String, String, String, String, String, String) {
    let Ok(tagged) = Probe::open(path).and_then(|p| p.read()) else {
        return Default::default();
    };
    let duration = tagged.properties().duration().as_secs_f64();
    let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) else {
        return (
            duration,
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
        );
    };
    (
        duration,
        tag.get_string(&ItemKey::Comment)
            .unwrap_or_default()
            .to_string(),
        tag.artist().map(|x| x.into_owned()).unwrap_or_default(),
        tag.album().map(|x| x.into_owned()).unwrap_or_default(),
        tag.genre().map(|x| x.into_owned()).unwrap_or_default(),
        tag.year().map(|x| x.to_string()).unwrap_or_default(),
        tag.get_string(&ItemKey::PodcastKeywords)
            .unwrap_or_default()
            .to_string(),
    )
}

fn riff_ixml(path: &Path) -> Option<String> {
    let data = fs::read(path).ok()?;
    if data.len() < 12 || &data[0..4] != b"RIFF" || &data[8..12] != b"WAVE" {
        return None;
    }
    let mut p = 12usize;
    while p + 8 <= data.len() {
        let size = u32::from_le_bytes(data[p + 4..p + 8].try_into().ok()?) as usize;
        let end = p + 8 + size;
        if end > data.len() {
            break;
        }
        if &data[p..p + 4] == b"iXML" {
            return Some(
                String::from_utf8_lossy(&data[p + 8..end])
                    .trim_end_matches('\0')
                    .to_string(),
            );
        }
        p = end + (size & 1)
    }
    None
}

fn xml_value(xml: &str, name: &str) -> String {
    let Ok(doc) = roxmltree::Document::parse(xml) else {
        return String::new();
    };
    doc.descendants()
        .find(|n| n.is_element() && n.tag_name().name().eq_ignore_ascii_case(name))
        .and_then(|n| n.text())
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn parse_ucs_filename(path: &Path) -> UcsMeta {
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let mut u = UcsMeta::default();
    if let Some((cat, rest)) = stem.split_once('_') {
        u.cat_id = cat.to_string();
        let core = rest.split('_').next().unwrap_or(rest);
        u.fx_name = core
            .rsplit_once('-')
            .map(|(_, x)| x)
            .unwrap_or(core)
            .trim()
            .to_string()
    }
    u
}

fn read_ucs(path: &Path) -> UcsMeta {
    let mut u = parse_ucs_filename(path);
    let Some(xml) = riff_ixml(path) else { return u };
    macro_rules! set {
        ($field:ident,$tag:expr) => {{
            let v = xml_value(&xml, $tag);
            if !v.is_empty() {
                u.$field = v;
            }
        }};
    }
    set!(cat_id, "cat_id");
    set!(category, "category");
    set!(sub_category, "sub_category");
    set!(user_category, "user_category");
    set!(vendor_category, "vendor_category");
    set!(fx_name, "fx_name");
    set!(creator_id, "creator_id");
    set!(source_id, "source_id");
    set!(user_data, "user_data");
    set!(ucs_library, "library");
    u
}

fn escape_xml(v: &str) -> String {
    v.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
fn aswg_xml(u: &UcsMeta) -> String {
    format!("<ASWG><content_type>sfx</content_type><category>{}</category><sub_category>{}</sub_category><cat_id>{}</cat_id><user_category>{}</user_category><vendor_category>{}</vendor_category><fx_name>{}</fx_name><library>{}</library><creator_id>{}</creator_id><source_id>{}</source_id><user_data>{}</user_data></ASWG>",escape_xml(&u.category),escape_xml(&u.sub_category),escape_xml(&u.cat_id),escape_xml(&u.user_category),escape_xml(&u.vendor_category),escape_xml(&u.fx_name),escape_xml(&u.ucs_library),escape_xml(&u.creator_id),escape_xml(&u.source_id),escape_xml(&u.user_data))
}
fn replace_ascii_ci(
    hay: &str,
    start_tag: &str,
    end_tag: &str,
    replacement: &str,
) -> Option<String> {
    let low = hay.to_ascii_lowercase();
    let a = low.find(&start_tag.to_ascii_lowercase())?;
    let b = low[a..].find(&end_tag.to_ascii_lowercase())? + a + end_tag.len();
    Some(format!("{}{}{}", &hay[..a], replacement, &hay[b..]))
}
fn merge_ixml(old: Option<String>, u: &UcsMeta) -> String {
    let aswg = aswg_xml(u);
    match old {
        Some(x) => replace_ascii_ci(&x, "<ASWG", "</ASWG>", &aswg)
            .or_else(|| {
                x.rfind("</BWFXML>")
                    .map(|p| format!("{}{}{}", &x[..p], aswg, &x[p..]))
            })
            .unwrap_or_else(|| format!("<BWFXML>{aswg}</BWFXML>")),
        None => format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><BWFXML>{aswg}</BWFXML>"),
    }
}

fn write_ixml(path: &Path, u: &UcsMeta) -> Result<(), String> {
    let mut data = fs::read(path).map_err(|e| e.to_string())?;
    if data.len() < 12 || &data[0..4] != b"RIFF" || &data[8..12] != b"WAVE" {
        return Ok(());
    }
    let xml = merge_ixml(riff_ixml(path), u);
    let bytes = xml.as_bytes();
    let mut chunk = Vec::with_capacity(bytes.len() + 9);
    chunk.extend_from_slice(b"iXML");
    chunk.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    chunk.extend_from_slice(bytes);
    if bytes.len() % 2 == 1 {
        chunk.push(0)
    }
    let mut p = 12usize;
    let mut range = None;
    while p + 8 <= data.len() {
        let size =
            u32::from_le_bytes(data[p + 4..p + 8].try_into().map_err(|_| "Invalid RIFF")?) as usize;
        let end = p + 8 + size + (size & 1);
        if end > data.len() {
            break;
        }
        if &data[p..p + 4] == b"iXML" {
            range = Some((p, end));
            break;
        }
        p = end
    }
    if let Some((a, b)) = range {
        data.splice(a..b, chunk);
    } else {
        data.extend_from_slice(&chunk)
    }
    if data.len() > u32::MAX as usize {
        return Err("RF64 metadata writing is not supported yet".into());
    }
    let riff_size = (data.len() - 8) as u32;
    data[4..8].copy_from_slice(&riff_size.to_le_bytes());
    let mut f = fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    f.write_all(&data).map_err(|e| e.to_string())
}

#[tauri::command]
fn scan_library(path: String, db: State<Db>) -> Result<usize, String> {
    let root = PathBuf::from(&path);
    if !root.is_dir() {
        return Err("Folder does not exist".into());
    }
    let stamp = now();
    let lib_name = root
        .file_name()
        .map(|x| x.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.clone());
    let c = db.0.lock();
    c.execute("INSERT INTO libraries(path,name,added_at) VALUES(?1,?2,?3) ON CONFLICT(path) DO UPDATE SET name=excluded.name",params![path,lib_name,stamp]).map_err(|e|e.to_string())?;
    let lib_id: i64 = c
        .query_row(
            "SELECT id FROM libraries WHERE path=?1",
            [text_path(&root)],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    let mut count = 0;
    for e in WalkDir::new(&root)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
    {
        let p = e.path();
        let ext = p
            .extension()
            .and_then(|x| x.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !AUDIO_EXTS.contains(&ext.as_str()) {
            continue;
        }
        let (duration, description, artist, album, genre, year, tags) = read_generic(p);
        let u = read_ucs(p);
        let name = p
            .file_name()
            .map(|x| x.to_string_lossy().into_owned())
            .unwrap_or_default();
        c.execute("INSERT INTO audio(path,name,ext,library_id,description,duration,artist,album,genre,year,tags,scanned_at,cat_id,category,sub_category,user_category,vendor_category,fx_name,creator_id,source_id,user_data,ucs_library) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22) ON CONFLICT(path) DO UPDATE SET name=excluded.name,ext=excluded.ext,library_id=excluded.library_id,description=excluded.description,duration=excluded.duration,artist=excluded.artist,album=excluded.album,genre=excluded.genre,year=excluded.year,tags=excluded.tags,scanned_at=excluded.scanned_at,cat_id=excluded.cat_id,category=excluded.category,sub_category=excluded.sub_category,user_category=excluded.user_category,vendor_category=excluded.vendor_category,fx_name=excluded.fx_name,creator_id=excluded.creator_id,source_id=excluded.source_id,user_data=excluded.user_data,ucs_library=excluded.ucs_library",params![text_path(p),name,ext,lib_id,description,duration,artist,album,genre,year,tags,stamp,u.cat_id,u.category,u.sub_category,u.user_category,u.vendor_category,u.fx_name,u.creator_id,u.source_id,u.user_data,u.ucs_library]).map_err(|e|e.to_string())?;
        count += 1;
    }
    c.execute(
        "DELETE FROM audio WHERE library_id=?1 AND scanned_at<?2",
        params![lib_id, stamp],
    )
    .map_err(|e| e.to_string())?;
    Ok(count)
}

fn finish_scan(status: &Arc<Mutex<ScanStatus>>, phase: &str, message: String) {
    let mut s = status.lock();
    s.running = false;
    s.phase = phase.into();
    s.message = message
}
fn run_scan_job(
    root: PathBuf,
    db_path: PathBuf,
    status: Arc<Mutex<ScanStatus>>,
    cancel: Arc<AtomicBool>,
) {
    if !root.is_dir() {
        finish_scan(&status, "error", "Folder does not exist".into());
        return;
    }
    let mut files = Vec::new();
    for entry in WalkDir::new(&root).follow_links(false).into_iter() {
        if cancel.load(Ordering::Relaxed) {
            finish_scan(&status, "cancelled", String::new());
            return;
        }
        let Ok(e) = entry else { continue };
        if !e.file_type().is_file() {
            continue;
        }
        let ext = e
            .path()
            .extension()
            .and_then(|x| x.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if AUDIO_EXTS.contains(&ext.as_str()) {
            files.push(e.path().to_path_buf());
            let mut s = status.lock();
            s.phase = "counting".into();
            s.current = files.len();
            s.path = text_path(e.path())
        }
    }
    {
        let mut s = status.lock();
        s.phase = "scanning".into();
        s.current = 0;
        s.total = files.len();
        s.path = text_path(&root)
    }
    let Ok(mut conn) = Connection::open(&db_path) else {
        finish_scan(&status, "error", "Cannot open database".into());
        return;
    };
    let Ok(tx) = conn.transaction() else {
        finish_scan(&status, "error", "Cannot begin database transaction".into());
        return;
    };
    let stamp = now();
    let root_text = text_path(&root);
    let lib_name = root
        .file_name()
        .map(|x| x.to_string_lossy().into_owned())
        .unwrap_or_else(|| root_text.clone());
    if let Err(e)=tx.execute("INSERT INTO libraries(path,name,added_at) VALUES(?1,?2,?3) ON CONFLICT(path) DO UPDATE SET name=excluded.name",params![root_text,lib_name,stamp]){finish_scan(&status,"error",e.to_string());return}
    let lib_id = match tx.query_row(
        "SELECT id FROM libraries WHERE path=?1",
        [text_path(&root)],
        |r| r.get::<_, i64>(0),
    ) {
        Ok(v) => v,
        Err(e) => {
            finish_scan(&status, "error", e.to_string());
            return;
        }
    };
    for (index, p) in files.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            drop(tx);
            finish_scan(&status, "cancelled", String::new());
            return;
        }
        let ext = p
            .extension()
            .and_then(|x| x.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let (duration, description, artist, album, genre, year, tags) = read_generic(p);
        let u = read_ucs(p);
        let name = p
            .file_name()
            .map(|x| x.to_string_lossy().into_owned())
            .unwrap_or_default();
        let result=tx.execute("INSERT INTO audio(path,name,ext,library_id,description,duration,artist,album,genre,year,tags,scanned_at,cat_id,category,sub_category,user_category,vendor_category,fx_name,creator_id,source_id,user_data,ucs_library) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22) ON CONFLICT(path) DO UPDATE SET name=excluded.name,ext=excluded.ext,library_id=excluded.library_id,description=excluded.description,duration=excluded.duration,artist=excluded.artist,album=excluded.album,genre=excluded.genre,year=excluded.year,tags=excluded.tags,scanned_at=excluded.scanned_at,cat_id=excluded.cat_id,category=excluded.category,sub_category=excluded.sub_category,user_category=excluded.user_category,vendor_category=excluded.vendor_category,fx_name=excluded.fx_name,creator_id=excluded.creator_id,source_id=excluded.source_id,user_data=excluded.user_data,ucs_library=excluded.ucs_library",params![text_path(p),name,ext,lib_id,description,duration,artist,album,genre,year,tags,stamp,u.cat_id,u.category,u.sub_category,u.user_category,u.vendor_category,u.fx_name,u.creator_id,u.source_id,u.user_data,u.ucs_library]);
        if let Err(e) = result {
            drop(tx);
            finish_scan(&status, "error", e.to_string());
            return;
        }
        {
            let mut s = status.lock();
            s.current = index + 1;
            s.path = text_path(p)
        }
    }
    if cancel.load(Ordering::Relaxed) {
        drop(tx);
        finish_scan(&status, "cancelled", String::new());
        return;
    }
    if let Err(e) = tx.execute(
        "DELETE FROM audio WHERE library_id=?1 AND scanned_at<?2",
        params![lib_id, stamp],
    ) {
        drop(tx);
        finish_scan(&status, "error", e.to_string());
        return;
    }
    if let Err(e) = tx.commit() {
        finish_scan(&status, "error", e.to_string());
        return;
    }
    finish_scan(&status, "completed", files.len().to_string())
}

#[tauri::command]
fn start_scan(path: String, scan: State<ScanManager>) -> Result<(), String> {
    {
        let mut s = scan.status.lock();
        if s.running {
            return Err("A scan is already running".into());
        }
        *s = ScanStatus {
            running: true,
            phase: "counting".into(),
            current: 0,
            total: 0,
            path: path.clone(),
            message: String::new(),
        }
    }
    scan.cancel.store(false, Ordering::Relaxed);
    let status = scan.status.clone();
    let cancel = scan.cancel.clone();
    let db_path = scan.db_path.clone();
    thread::spawn(move || run_scan_job(PathBuf::from(path), db_path, status, cancel));
    Ok(())
}
#[tauri::command]
fn get_scan_status(scan: State<ScanManager>) -> ScanStatus {
    scan.status.lock().clone()
}
#[tauri::command]
fn cancel_scan(scan: State<ScanManager>) -> bool {
    let running = scan.status.lock().running;
    if running {
        scan.cancel.store(true, Ordering::Relaxed)
    }
    running
}

#[tauri::command]
fn remove_library(id: i64, db: State<Db>) -> Result<(), String> {
    let c = db.0.lock();
    c.execute("DELETE FROM libraries WHERE id=?1", [id])
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn export_database(destination: String, db: State<Db>) -> Result<(), String> {
    let c = db.0.lock();
    c.execute_batch("PRAGMA wal_checkpoint(FULL)")
        .map_err(|e| e.to_string())?;
    let source: String = c
        .query_row(
            "SELECT file FROM pragma_database_list WHERE name='main'",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    fs::copy(source, destination).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn list_libraries(db: State<Db>) -> Result<Vec<Library>, String> {
    let c = db.0.lock();
    let mut s = c
        .prepare("SELECT id,path,name FROM libraries ORDER BY name COLLATE NOCASE")
        .map_err(|e| e.to_string())?;
    let rows = s
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(rows
        .into_iter()
        .map(|(id, path, name)| Library {
            id,
            exists: Path::new(&path).is_dir(),
            path,
            name,
        })
        .collect())
}

#[tauri::command]
fn list_audio(view: String, db: State<Db>) -> Result<Vec<AudioItem>, String> {
    let c = db.0.lock();
    let filter = match view.as_str() {
        "playlist" => " WHERE a.playlist=1",
        "favorites" => " WHERE a.favorite=1",
        "recent" => " WHERE a.scanned_at >= strftime('%s','now')-604800",
        _ => "",
    };
    let sql=format!("SELECT a.id,a.path,a.name,a.ext,l.name,a.description,a.duration,a.rating,a.artist,a.album,a.genre,a.year,a.tags,a.scanned_at,a.favorite,a.cat_id,a.category,a.sub_category,a.user_category,a.vendor_category,a.fx_name,a.creator_id,a.source_id,a.user_data,a.ucs_library FROM audio a JOIN libraries l ON l.id=a.library_id{} ORDER BY a.name COLLATE NOCASE",filter);
    let mut s = c.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = s
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, f64>(6)?,
                r.get::<_, f64>(7)?,
                r.get::<_, String>(8)?,
                r.get::<_, String>(9)?,
                r.get::<_, String>(10)?,
                r.get::<_, String>(11)?,
                r.get::<_, String>(12)?,
                r.get::<_, i64>(13)?,
                r.get::<_, bool>(14)?,
                r.get::<_, String>(15)?,
                r.get::<_, String>(16)?,
                r.get::<_, String>(17)?,
                r.get::<_, String>(18)?,
                r.get::<_, String>(19)?,
                r.get::<_, String>(20)?,
                r.get::<_, String>(21)?,
                r.get::<_, String>(22)?,
                r.get::<_, String>(23)?,
                r.get::<_, String>(24)?,
            ))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(rows
        .into_iter()
        .map(|r| {
            let library_exists = Path::new(&r.1).parent().map(Path::is_dir).unwrap_or(false);
            let path_exists = Path::new(&r.1).is_file();
            AudioItem {
                id: r.0,
                path: r.1,
                name: r.2,
                ext: r.3,
                library: r.4,
                description: r.5,
                duration: r.6,
                rating: r.7,
                artist: r.8,
                album: r.9,
                genre: r.10,
                year: r.11,
                tags: r.12,
                scanned_at: r.13,
                favorite: r.14,
                cat_id: r.15,
                category: r.16,
                sub_category: r.17,
                user_category: r.18,
                vendor_category: r.19,
                fx_name: r.20,
                creator_id: r.21,
                source_id: r.22,
                user_data: r.23,
                ucs_library: r.24,
                path_exists,
                library_exists,
            }
        })
        .collect())
}

#[tauri::command]
fn copy_files(ids: Vec<i64>, destination: String, db: State<Db>) -> Result<usize, String> {
    let dest = PathBuf::from(destination);
    fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
    let c = db.0.lock();
    let mut n = 0;
    for id in ids {
        let p: Option<String> = c
            .query_row("SELECT path FROM audio WHERE id=?1", [id], |r| r.get(0))
            .optional()
            .map_err(|e| e.to_string())?;
        if let Some(p) = p {
            let src = PathBuf::from(p);
            if let Some(name) = src.file_name() {
                let mut target = dest.join(name);
                if target.exists() {
                    let stem = target.file_stem().unwrap_or_default().to_string_lossy();
                    let ext = target
                        .extension()
                        .map(|x| format!(".{}", x.to_string_lossy()))
                        .unwrap_or_default();
                    for i in 2..10000 {
                        let v = dest.join(format!("{} ({}){}", stem, i, ext));
                        if !v.exists() {
                            target = v;
                            break;
                        }
                    }
                }
                fs::copy(src, target).map_err(|e| e.to_string())?;
                n += 1
            }
        }
    }
    Ok(n)
}
#[tauri::command]
fn reveal_file(id: i64, db: State<Db>) -> Result<(), String> {
    let c = db.0.lock();
    let path: String = c
        .query_row("SELECT path FROM audio WHERE id=?1", [id], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    drop(c);
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .args(["/select,", &path])
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .args(["-R", &path])
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}
#[tauri::command]
fn set_playlist(ids: Vec<i64>, value: bool, db: State<Db>) -> Result<(), String> {
    let mut c = db.0.lock();
    let tx = c.transaction().map_err(|e| e.to_string())?;
    for id in ids {
        tx.execute(
            "UPDATE audio SET playlist=?1 WHERE id=?2",
            params![value, id],
        )
        .map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())
}
#[tauri::command]
fn remove_from_index(ids: Vec<i64>, db: State<Db>) -> Result<(), String> {
    let mut c = db.0.lock();
    let tx = c.transaction().map_err(|e| e.to_string())?;
    for id in ids {
        tx.execute("DELETE FROM audio WHERE id=?1", [id])
            .map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())
}

#[tauri::command]
fn update_metadata(
    id: i64,
    metadata: HashMap<String, String>,
    db: State<Db>,
) -> Result<(), String> {
    let c = db.0.lock();
    let path: String = c
        .query_row("SELECT path FROM audio WHERE id=?1", [id], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let get = |k: &str| metadata.get(k).cloned().unwrap_or_default();
    let rating = get("rating").parse::<f64>().unwrap_or(0.0).clamp(0.0, 5.0);
    let u = UcsMeta {
        cat_id: get("cat_id"),
        category: get("category"),
        sub_category: get("sub_category"),
        user_category: get("user_category"),
        vendor_category: get("vendor_category"),
        fx_name: get("fx_name"),
        creator_id: get("creator_id"),
        source_id: get("source_id"),
        user_data: get("user_data"),
        ucs_library: get("ucs_library"),
    };
    c.execute("UPDATE audio SET description=?1,rating=?2,tags=?3,cat_id=?4,category=?5,sub_category=?6,user_category=?7,vendor_category=?8,fx_name=?9,creator_id=?10,source_id=?11,user_data=?12,ucs_library=?13 WHERE id=?14",params![get("description"),rating,get("tags"),u.cat_id,u.category,u.sub_category,u.user_category,u.vendor_category,u.fx_name,u.creator_id,u.source_id,u.user_data,u.ucs_library,id]).map_err(|e|e.to_string())?;
    if let Ok(mut tagged) = Probe::open(&path).and_then(|p| p.read()) {
        let ty = tagged.primary_tag_type();
        if tagged.primary_tag().is_none() {
            tagged.insert_tag(Tag::new(ty));
        }
        if let Some(tag) = tagged.primary_tag_mut() {
            tag.insert_text(ItemKey::Comment, get("description"));
            tag.insert_text(ItemKey::PodcastKeywords, get("tags"));
            let _ = tagged.save_to_path(&path, lofty::config::WriteOptions::default());
        }
    }
    if Path::new(&path)
        .extension()
        .and_then(|x| x.to_str())
        .map(|x| x.eq_ignore_ascii_case("wav") || x.eq_ignore_ascii_case("wave"))
        .unwrap_or(false)
    {
        write_ixml(Path::new(&path), &u)?
    }
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let path = db_path(app.handle());
            let c = init_db(app.handle()).map_err(std::io::Error::other)?;
            app.manage(Db(Mutex::new(c)));
            app.manage(ScanManager {
                status: Arc::new(Mutex::new(ScanStatus::default())),
                cancel: Arc::new(AtomicBool::new(false)),
                db_path: path,
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            start_scan,
            get_scan_status,
            cancel_scan,
            scan_library,
            list_libraries,
            list_audio,
            copy_files,
            reveal_file,
            set_playlist,
            remove_from_index,
            remove_library,
            export_database,
            update_metadata
        ])
        .run(tauri::generate_context!())
        .expect("error while running Lings")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn writes_and_reads_ucs_ixml() {
        let path = std::env::temp_dir().join(format!("lings_ucs_{}.wav", now()));
        let mut wav=b"RIFF\x24\0\0\0WAVEfmt \x10\0\0\0\x01\0\x01\0\x40\x1f\0\0\x80\x3e\0\0\x02\0\x10\0data\0\0\0\0".to_vec();
        let size = (wav.len() - 8) as u32;
        wav[4..8].copy_from_slice(&size.to_le_bytes());
        fs::write(&path, &wav).unwrap();
        let u = UcsMeta {
            cat_id: "AIRBlow".into(),
            category: "AIR".into(),
            sub_category: "BLOW".into(),
            fx_name: "Compressed Air Burst".into(),
            user_data: "中文数据 & safe".into(),
            ..Default::default()
        };
        write_ixml(&path, &u).unwrap();
        let got = read_ucs(&path);
        assert_eq!(got.cat_id, "AIRBlow");
        assert_eq!(got.fx_name, "Compressed Air Burst");
        assert_eq!(got.user_data, "中文数据 & safe");
        fs::remove_file(path).unwrap();
    }
    #[test]
    fn preserves_other_ixml_fields() {
        let old =
            "<BWFXML><PROJECT>Game</PROJECT><ASWG><cat_id>OLD</cat_id></ASWG></BWFXML>".to_string();
        let out = merge_ixml(
            Some(old),
            &UcsMeta {
                cat_id: "NEW".into(),
                ..Default::default()
            },
        );
        assert!(out.contains("<PROJECT>Game</PROJECT>"));
        assert_eq!(xml_value(&out, "cat_id"), "NEW")
    }
    #[test]
    fn detects_standard_install_locations() {
        assert!(is_installed_dir(Path::new("C:/Program Files/Lings")));
        assert!(is_installed_dir(Path::new(
            "/Applications/Lings.app/Contents/MacOS"
        )));
        assert!(!is_installed_dir(Path::new("D:/Portable/Lings")))
    }
    #[test]
    fn cancelled_scan_rolls_back_every_change() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("lings_cancel_{suffix}"));
        fs::create_dir_all(&root).unwrap();
        for i in 0..800 {
            fs::write(root.join(format!("AIRBlow_test-{i}.wav")), b"not-a-wave").unwrap()
        }
        let db = root.join("test.db");
        let c = Connection::open(&db).unwrap();
        c.execute_batch("CREATE TABLE libraries(id INTEGER PRIMARY KEY,path TEXT UNIQUE,name TEXT,added_at INTEGER);CREATE TABLE audio(id INTEGER PRIMARY KEY,path TEXT UNIQUE,name TEXT,ext TEXT,library_id INTEGER,description TEXT DEFAULT '',duration REAL DEFAULT 0,rating REAL DEFAULT 0,artist TEXT DEFAULT '',album TEXT DEFAULT '',genre TEXT DEFAULT '',year TEXT DEFAULT '',tags TEXT DEFAULT '',scanned_at INTEGER,playlist INTEGER DEFAULT 0,favorite INTEGER DEFAULT 0,cat_id TEXT DEFAULT '',category TEXT DEFAULT '',sub_category TEXT DEFAULT '',user_category TEXT DEFAULT '',vendor_category TEXT DEFAULT '',fx_name TEXT DEFAULT '',creator_id TEXT DEFAULT '',source_id TEXT DEFAULT '',user_data TEXT DEFAULT '',ucs_library TEXT DEFAULT '',FOREIGN KEY(library_id) REFERENCES libraries(id) ON DELETE CASCADE);").unwrap();
        drop(c);
        let status = Arc::new(Mutex::new(ScanStatus::default()));
        let cancel = Arc::new(AtomicBool::new(false));
        let s = status.clone();
        let flag = cancel.clone();
        let r = root.clone();
        let d = db.clone();
        let worker = thread::spawn(move || run_scan_job(r, d, s, flag));
        for _ in 0..500 {
            let snapshot = status.lock().clone();
            if snapshot.phase == "scanning" && snapshot.current > 0 {
                break;
            }
            thread::sleep(std::time::Duration::from_millis(2))
        }
        cancel.store(true, Ordering::Relaxed);
        worker.join().unwrap();
        let c = Connection::open(&db).unwrap();
        let audio: i64 = c
            .query_row("SELECT count(*) FROM audio", [], |r| r.get(0))
            .unwrap();
        let libs: i64 = c
            .query_row("SELECT count(*) FROM libraries", [], |r| r.get(0))
            .unwrap();
        assert_eq!((audio, libs), (0, 0));
        assert_eq!(status.lock().phase, "cancelled");
        drop(c);
        fs::remove_dir_all(root).unwrap();
    }
}
