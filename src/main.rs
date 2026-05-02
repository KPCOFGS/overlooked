use dioxus::prelude::*;
use futures_util::StreamExt;
use reqwest::Client;
use rusqlite::{params, Connection, Row};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use uuid::Uuid;

const FAVICON: Asset = asset!("/assets/favicon.ico");
const MAIN_CSS: Asset = asset!("/assets/main.css");
const APP_ICON_SVG: Asset = asset!("/assets/logo.svg");

const MAX_HISTORY_MESSAGES: i64 = 10000;
// Maximum title length for chat rename (in characters, not bytes)
const MAX_TITLE_LEN: usize = 200;
// Maximum message length (characters)
const MAX_MESSAGE_CHARS: usize = 200_000;
// Maximum length for short text settings (api key, base url, model name)
const MAX_FIELD_LEN: usize = 512;
// Maximum length of system prompt
const MAX_SYSTEM_PROMPT_CHARS: usize = 50_000;
// Maximum length of a stop sequence comma-list
const MAX_STOP_FIELD_LEN: usize = 512;
// Maximum search query
const MAX_SEARCH_QUERY_CHARS: usize = 500;
// Default accent color (warm rust orange)
const DEFAULT_ACCENT: &str = "#ff7d3b";

fn main() {
    let cfg = build_desktop_config();
    dioxus::LaunchBuilder::desktop()
        .with_cfg(cfg)
        .launch(App);
}

/// Build Dioxus desktop config with a procedurally-rendered window icon.
/// Transparent background with two overlapping speech bubbles + typing dots.
fn build_desktop_config() -> dioxus::desktop::Config {
    use dioxus::desktop::tao::window::{Icon, WindowBuilder};
    use dioxus::desktop::Config;

    // Larger size for crisper rendering in dock/taskbar
    const W: u32 = 128;
    const H: u32 = 128;
    let mut buf = vec![0u8; (W * H * 4) as usize]; // alpha = 0 by default → transparent

    let in_rounded = |x: f32, y: f32, x0: f32, y0: f32, x1: f32, y1: f32, r: f32| -> bool {
        if x < x0 || x > x1 || y < y0 || y > y1 {
            return false;
        }
        let dx = if x < x0 + r {
            x0 + r - x
        } else if x > x1 - r {
            x - (x1 - r)
        } else {
            0.0
        };
        let dy = if y < y0 + r {
            y0 + r - y
        } else if y > y1 - r {
            y - (y1 - r)
        } else {
            0.0
        };
        dx * dx + dy * dy <= r * r
    };

    let in_triangle = |px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32, cx: f32, cy: f32| -> bool {
        let d1 = (px - bx) * (ay - by) - (ax - bx) * (py - by);
        let d2 = (px - cx) * (by - cy) - (bx - cx) * (py - cy);
        let d3 = (px - ax) * (cy - ay) - (cx - ax) * (py - ay);
        let has_neg = (d1 < 0.0) || (d2 < 0.0) || (d3 < 0.0);
        let has_pos = (d1 > 0.0) || (d2 > 0.0) || (d3 > 0.0);
        !(has_neg && has_pos)
    };

    let put = |buf: &mut Vec<u8>, x: u32, y: u32, r: u8, g: u8, b: u8| {
        let i = ((y * W + x) * 4) as usize;
        buf[i] = r;
        buf[i + 1] = g;
        buf[i + 2] = b;
        buf[i + 3] = 0xff;
    };

    for y in 0..H {
        for x in 0..W {
            let fx = x as f32 + 0.5;
            let fy = y as f32 + 0.5;

            // Back bubble (slate), with tail at bottom-left-ish
            let back_bubble = in_rounded(fx, fy, 18.0, 18.0, 86.0, 66.0, 8.0);
            let back_tail = in_triangle(fx, fy, 46.0, 64.0, 54.0, 64.0, 42.0, 78.0);
            if back_bubble || back_tail {
                put(&mut buf, x, y, 0x4d, 0x53, 0x65);
            }

            // Front bubble (rust orange), with tail at bottom-right
            let front_bubble = in_rounded(fx, fy, 38.0, 46.0, 110.0, 98.0, 8.0);
            let front_tail = in_triangle(fx, fy, 92.0, 96.0, 100.0, 96.0, 100.0, 112.0);
            if front_bubble || front_tail {
                put(&mut buf, x, y, 0xff, 0x7d, 0x3b);
            }

            // Three white dots in the front bubble
            for dot_cx in [58.0_f32, 74.0, 90.0] {
                let dx = fx - dot_cx;
                let dy = fy - 72.0;
                if dx * dx + dy * dy < 25.0 {
                    put(&mut buf, x, y, 0xff, 0xff, 0xff);
                }
            }
        }
    }

    let mut window = WindowBuilder::new().with_title("overlooked");
    if let Ok(icon) = Icon::from_rgba(buf, W, H) {
        window = window.with_window_icon(Some(icon));
    }
    Config::default().with_window(window)
}

/* ================= INPUT SANITIZATION ================= */

/// Truncate a string to at most `max_chars` characters without panicking on
/// multi-byte UTF-8 boundaries (which `String::truncate` does on a non-char-boundary).
fn safe_truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        s.chars().take(max_chars).collect()
    }
}

/// Strip ASCII control characters (except tab/newline) and truncate.
/// Use for "single-line" settings fields like API keys / base URLs / model names.
fn sanitize_field(s: &str, max_chars: usize) -> String {
    let cleaned: String = s
        .chars()
        .filter(|c| !c.is_control() || *c == '\t')
        .collect();
    safe_truncate(cleaned.trim(), max_chars)
}

/// Strip control chars except newline + tab; truncate. For multiline content
/// (system prompt, message body).
fn sanitize_multiline(s: &str, max_chars: usize) -> String {
    let cleaned: String = s
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect();
    safe_truncate(&cleaned, max_chars)
}

/// Validate a hex color "#rrggbb" (case-insensitive). Returns the lowercased
/// canonical form on success, or None if invalid.
fn parse_hex_color(s: &str) -> Option<String> {
    let s = s.trim();
    if s.len() != 7 || !s.starts_with('#') {
        return None;
    }
    if !s[1..].chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(s.to_lowercase())
}

/// Username rules: 3-32 chars, ASCII alphanumeric + underscore + hyphen.
fn validate_username(s: &str) -> Result<String, String> {
    let s = s.trim();
    if s.len() < 3 {
        return Err("Username must be at least 3 characters.".into());
    }
    if s.len() > 32 {
        return Err("Username must be at most 32 characters.".into());
    }
    if !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Err("Username can only contain letters, digits, _ and -.".into());
    }
    if s.eq_ignore_ascii_case("guest") {
        return Err("\"guest\" is reserved.".into());
    }
    Ok(s.to_string())
}

/// Password rules: 8-128 chars, must contain at least one letter and one digit,
/// no whitespace at start/end, no control characters.
fn validate_password(s: &str) -> Result<(), String> {
    let n = s.chars().count();
    if n < 8 {
        return Err("Password must be at least 8 characters.".into());
    }
    if n > 128 {
        return Err("Password must be at most 128 characters.".into());
    }
    if s.chars().any(|c| c.is_control()) {
        return Err("Password cannot contain control characters.".into());
    }
    if s.starts_with(char::is_whitespace) || s.ends_with(char::is_whitespace) {
        return Err("Password cannot begin or end with whitespace.".into());
    }
    if !s.chars().any(|c| c.is_alphabetic()) {
        return Err("Password must contain at least one letter.".into());
    }
    if !s.chars().any(|c| c.is_ascii_digit()) {
        return Err("Password must contain at least one digit.".into());
    }
    Ok(())
}

fn hash_password(plain: &str) -> Result<String, String> {
    use argon2::{
        password_hash::{PasswordHasher, SaltString},
        Argon2,
    };
    // Generate a salt from system time + a random uuid bit; argon2's salt only needs uniqueness
    let mut bytes = [0u8; 16];
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u128)
        .unwrap_or(0);
    let uuid = uuid::Uuid::new_v4();
    let uuid_bytes = uuid.as_bytes();
    for i in 0..16 {
        bytes[i] = uuid_bytes[i] ^ ((nanos >> (i * 8)) as u8);
    }
    let salt = SaltString::encode_b64(&bytes).map_err(|e| format!("salt failed: {e}"))?;
    Argon2::default()
        .hash_password(plain.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| format!("hash failed: {e}"))
}

fn verify_password(plain: &str, hash: &str) -> bool {
    use argon2::{password_hash::{PasswordHash, PasswordVerifier}, Argon2};
    let Ok(parsed) = PasswordHash::new(hash) else {
        return false;
    };
    Argon2::default()
        .verify_password(plain.as_bytes(), &parsed)
        .is_ok()
}

#[derive(Clone, Debug, PartialEq)]
struct User {
    id: i64,
    username: String,
    is_guest: bool,
    avatar_data: Option<String>, // data: URL (image/png;base64,...)
}

/// Deterministic accent color for a string (used for default avatars).
fn color_for(s: &str) -> String {
    let mut h: u32 = 5381;
    for b in s.as_bytes() {
        h = h.wrapping_mul(33).wrapping_add(*b as u32);
    }
    // Pick from a small palette of pleasant hues
    const PALETTE: &[&str] = &[
        "#ff7d3b", "#3ba3ff", "#7d3bff", "#ff3b8a", "#3bff7d", "#ffb43b",
        "#3bffce", "#b43bff", "#ff3b3b", "#3b8aff",
    ];
    PALETTE[(h as usize) % PALETTE.len()].to_string()
}

fn initial_for(s: &str) -> String {
    s.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_else(|| "?".into())
}

const MAX_AVATAR_BYTES: usize = 1_000_000; // 1 MB cap on the encoded image
const MAX_AVATAR_DATA_URL_LEN: usize = 1_500_000; // base64 ≈ 1.34× source

/// Detect a supported image MIME from magic bytes. Returns None for anything
/// we don't whitelist.
fn sniff_image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() >= 8 && &bytes[0..8] == b"\x89PNG\r\n\x1a\n" {
        return Some("image/png");
    }
    if bytes.len() >= 3 && &bytes[0..3] == b"\xff\xd8\xff" {
        return Some("image/jpeg");
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    if bytes.len() >= 6 && (&bytes[0..6] == b"GIF87a" || &bytes[0..6] == b"GIF89a") {
        return Some("image/gif");
    }
    None
}

/// Read a local file, validate it's a small image, and return a base64 data URL.
fn load_avatar_from_path(path: &std::path::Path) -> Result<String, String> {
    use base64::Engine;
    let metadata = std::fs::metadata(path).map_err(|e| format!("Cannot read file: {e}"))?;
    if !metadata.is_file() {
        return Err("Not a regular file.".into());
    }
    if metadata.len() as usize > MAX_AVATAR_BYTES {
        return Err(format!(
            "Image is too large ({} bytes). Maximum is {} KB.",
            metadata.len(),
            MAX_AVATAR_BYTES / 1024
        ));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("Cannot read file: {e}"))?;
    let mime = sniff_image_mime(&bytes).ok_or_else(|| {
        "Unsupported image type. Use PNG, JPEG, WEBP, or GIF.".to_string()
    })?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let data_url = format!("data:{};base64,{}", mime, encoded);
    if data_url.len() > MAX_AVATAR_DATA_URL_LEN {
        return Err("Image data exceeds the storage limit.".into());
    }
    Ok(data_url)
}

/* ================= DATABASE ================= */

fn init_db() -> Connection {
    let conn = Connection::open("chat.db").unwrap();

    conn.execute(
        "CREATE TABLE IF NOT EXISTS chats (
            id TEXT PRIMARY KEY,
            title TEXT NOT NULL
        )",
        [],
    )
    .unwrap();

    conn.execute(
        "ALTER TABLE chats ADD COLUMN pinned INTEGER DEFAULT 0",
        [],
    )
    .ok();

    conn.execute(
        "CREATE TABLE IF NOT EXISTS messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            chat_id TEXT NOT NULL,
            role TEXT NOT NULL,
            content TEXT NOT NULL,
            timestamp DATETIME DEFAULT CURRENT_TIMESTAMP
        )",
        [],
    )
    .unwrap();

    conn.execute(
        "CREATE TABLE IF NOT EXISTS settings (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            model TEXT NOT NULL,
            system_prompt TEXT,
            temperature REAL,
            top_p REAL,
            max_tokens INTEGER,
            zoom INTEGER,
            maximized INTEGER,
            window_width INTEGER,
            window_height INTEGER
        )",
        [],
    )
    .unwrap();

    // Idempotent migrations for older chat.db files
    conn.execute(
        "ALTER TABLE settings ADD COLUMN provider TEXT DEFAULT 'ollama'",
        [],
    )
    .ok();
    conn.execute(
        "ALTER TABLE settings ADD COLUMN api_base TEXT DEFAULT 'http://localhost:11434'",
        [],
    )
    .ok();
    conn.execute(
        "ALTER TABLE settings ADD COLUMN api_key TEXT DEFAULT ''",
        [],
    )
    .ok();
    conn.execute(
        "ALTER TABLE settings ADD COLUMN theme TEXT DEFAULT 'dark'",
        [],
    )
    .ok();
    conn.execute(
        "ALTER TABLE settings ADD COLUMN presence_penalty REAL DEFAULT 0.0",
        [],
    )
    .ok();
    conn.execute(
        "ALTER TABLE settings ADD COLUMN frequency_penalty REAL DEFAULT 0.0",
        [],
    )
    .ok();
    conn.execute("ALTER TABLE settings ADD COLUMN seed INTEGER DEFAULT -1", [])
        .ok();
    conn.execute(
        "ALTER TABLE settings ADD COLUMN stop_sequences TEXT DEFAULT ''",
        [],
    )
    .ok();
    conn.execute("ALTER TABLE settings ADD COLUMN top_k INTEGER DEFAULT 0", [])
        .ok();
    conn.execute(
        "ALTER TABLE settings ADD COLUMN sidebar_collapsed INTEGER DEFAULT 0",
        [],
    )
    .ok();
    conn.execute(
        "ALTER TABLE settings ADD COLUMN accent_color TEXT DEFAULT '#ff7d3b'",
        [],
    )
    .ok();
    conn.execute(
        "ALTER TABLE settings ADD COLUMN tavily_api_key TEXT DEFAULT ''",
        [],
    )
    .ok();
    conn.execute(
        "ALTER TABLE settings ADD COLUMN web_search_enabled INTEGER DEFAULT 0",
        [],
    )
    .ok();

    // Users table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS users (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            username TEXT UNIQUE NOT NULL,
            password_hash TEXT,
            avatar_data TEXT,
            is_guest INTEGER NOT NULL DEFAULT 0,
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP
        )",
        [],
    )
    .unwrap();

    // App-wide state (which user is currently active)
    conn.execute(
        "CREATE TABLE IF NOT EXISTS app_state (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            current_user_id INTEGER NOT NULL DEFAULT 1
        )",
        [],
    )
    .unwrap();

    // Per-user settings (replaces the single-row `settings` table)
    conn.execute(
        "CREATE TABLE IF NOT EXISTS user_settings (
            user_id INTEGER PRIMARY KEY,
            model TEXT NOT NULL DEFAULT '',
            system_prompt TEXT NOT NULL DEFAULT '',
            temperature REAL NOT NULL DEFAULT 0.7,
            top_p REAL NOT NULL DEFAULT 0.95,
            max_tokens INTEGER NOT NULL DEFAULT 2048,
            zoom INTEGER NOT NULL DEFAULT 115,
            maximized INTEGER NOT NULL DEFAULT 1,
            window_width INTEGER NOT NULL DEFAULT 1024,
            window_height INTEGER NOT NULL DEFAULT 768,
            provider TEXT NOT NULL DEFAULT 'ollama',
            api_base TEXT NOT NULL DEFAULT 'http://localhost:11434',
            api_key TEXT NOT NULL DEFAULT '',
            theme TEXT NOT NULL DEFAULT 'light',
            presence_penalty REAL NOT NULL DEFAULT 0.0,
            frequency_penalty REAL NOT NULL DEFAULT 0.0,
            seed INTEGER NOT NULL DEFAULT -1,
            stop_sequences TEXT NOT NULL DEFAULT '',
            top_k INTEGER NOT NULL DEFAULT 0,
            sidebar_collapsed INTEGER NOT NULL DEFAULT 0,
            accent_color TEXT NOT NULL DEFAULT '#ff7d3b',
            tavily_api_key TEXT NOT NULL DEFAULT '',
            web_search_enabled INTEGER NOT NULL DEFAULT 0
        )",
        [],
    )
    .unwrap();

    // user_id on chats — DEFAULT 1 (guest)
    conn.execute(
        "ALTER TABLE chats ADD COLUMN user_id INTEGER NOT NULL DEFAULT 1",
        [],
    )
    .ok();

    // Ensure guest user exists with id = 1
    conn.execute(
        "INSERT OR IGNORE INTO users (id, username, password_hash, is_guest) VALUES (1, 'guest', NULL, 1)",
        [],
    )
    .unwrap();

    // Ensure app_state row exists
    conn.execute(
        "INSERT OR IGNORE INTO app_state (id, current_user_id) VALUES (1, 1)",
        [],
    )
    .unwrap();

    // Migrate legacy `settings` row (id=1) into user_settings if guest doesn't have one yet.
    let guest_has_settings: bool = conn
        .prepare("SELECT EXISTS(SELECT 1 FROM user_settings WHERE user_id = 1)")
        .unwrap()
        .query_row([], |r| r.get(0))
        .unwrap_or(false);
    if !guest_has_settings {
        // Try to copy from legacy settings table (if present and has row)
        let copied = conn.execute(
            "INSERT INTO user_settings (user_id, model, system_prompt, temperature, top_p, max_tokens, zoom, maximized, window_width, window_height, provider, api_base, api_key, theme, presence_penalty, frequency_penalty, seed, stop_sequences, top_k, sidebar_collapsed, accent_color, tavily_api_key, web_search_enabled)
             SELECT 1, COALESCE(model, ''), COALESCE(system_prompt, ''), COALESCE(temperature, 0.7), COALESCE(top_p, 0.95), COALESCE(max_tokens, 2048), COALESCE(zoom, 115), COALESCE(maximized, 1), COALESCE(window_width, 1024), COALESCE(window_height, 768), COALESCE(provider, 'ollama'), COALESCE(api_base, 'http://localhost:11434'), COALESCE(api_key, ''), COALESCE(theme, 'light'), COALESCE(presence_penalty, 0.0), COALESCE(frequency_penalty, 0.0), COALESCE(seed, -1), COALESCE(stop_sequences, ''), COALESCE(top_k, 0), COALESCE(sidebar_collapsed, 0), COALESCE(accent_color, '#ff7d3b'), COALESCE(tavily_api_key, ''), COALESCE(web_search_enabled, 0)
             FROM settings WHERE id = 1",
            [],
        );
        if copied.is_err() || copied.unwrap() == 0 {
            // No legacy row — insert defaults for guest
            conn.execute(
                "INSERT INTO user_settings (user_id) VALUES (1)",
                [],
            )
            .unwrap();
        }
    }

    conn
}

/* ================= USER & AUTH DB HELPERS ================= */

fn current_user_id(conn: &Connection) -> i64 {
    conn.query_row("SELECT current_user_id FROM app_state WHERE id = 1", [], |r| r.get(0))
        .unwrap_or(1)
}

fn set_current_user_id(conn: &Connection, uid: i64) {
    conn.execute("UPDATE app_state SET current_user_id = ?1 WHERE id = 1", params![uid])
        .ok();
}

fn load_user(conn: &Connection, uid: i64) -> User {
    conn.query_row(
        "SELECT id, username, COALESCE(avatar_data, ''), is_guest FROM users WHERE id = ?1",
        params![uid],
        |r| {
            let avatar: String = r.get(2)?;
            Ok(User {
                id: r.get(0)?,
                username: r.get(1)?,
                avatar_data: if avatar.is_empty() { None } else { Some(avatar) },
                is_guest: r.get::<_, i64>(3)? != 0,
            })
        },
    )
    .unwrap_or(User {
        id: 1,
        username: "guest".to_string(),
        avatar_data: None,
        is_guest: true,
    })
}

fn list_users(conn: &Connection) -> Vec<User> {
    let mut out = Vec::new();
    let mut stmt = conn
        .prepare("SELECT id, username, COALESCE(avatar_data, ''), is_guest FROM users ORDER BY is_guest DESC, username")
        .unwrap();
    let rows = stmt.query_map([], |r| {
        let avatar: String = r.get(2)?;
        Ok(User {
            id: r.get(0)?,
            username: r.get(1)?,
            avatar_data: if avatar.is_empty() { None } else { Some(avatar) },
            is_guest: r.get::<_, i64>(3)? != 0,
        })
    });
    if let Ok(rows) = rows {
        for r in rows.flatten() { out.push(r); }
    }
    out
}

fn create_user(conn: &Connection, username: &str, password: &str) -> Result<i64, String> {
    let username = validate_username(username)?;
    validate_password(password)?;
    let hash = hash_password(password)?;
    conn.execute(
        "INSERT INTO users (username, password_hash, is_guest) VALUES (?1, ?2, 0)",
        params![username, hash],
    )
    .map_err(|e| {
        if e.to_string().contains("UNIQUE") {
            "That username is already taken.".to_string()
        } else {
            format!("Database error: {e}")
        }
    })?;
    let id = conn.last_insert_rowid();
    // Seed a row in user_settings
    conn.execute(
        "INSERT INTO user_settings (user_id) VALUES (?1)",
        params![id],
    )
    .ok();
    Ok(id)
}

fn login_user(conn: &Connection, username: &str, password: &str) -> Result<i64, String> {
    let username = validate_username(username).map_err(|_| "Wrong username or password.".to_string())?;
    let row: Result<(i64, String), _> = conn.query_row(
        "SELECT id, COALESCE(password_hash, '') FROM users WHERE username = ?1 AND is_guest = 0",
        params![username],
        |r| Ok((r.get(0)?, r.get(1)?)),
    );
    let (id, hash) = row.map_err(|_| "Wrong username or password.".to_string())?;
    if hash.is_empty() || !verify_password(password, &hash) {
        return Err("Wrong username or password.".into());
    }
    Ok(id)
}

fn update_avatar(conn: &Connection, uid: i64, data: Option<String>) -> Result<(), String> {
    let data_owned = data.unwrap_or_default();
    conn.execute(
        "UPDATE users SET avatar_data = ?1 WHERE id = ?2",
        params![if data_owned.is_empty() { None } else { Some(data_owned) }, uid],
    )
    .map_err(|e| format!("DB error: {e}"))?;
    Ok(())
}

fn clamp_to_i32(v: i64) -> i32 {
    if v > i32::MAX as i64 {
        i32::MAX
    } else if v < i32::MIN as i64 {
        i32::MIN
    } else {
        v as i32
    }
}

fn clamp_f64(v: f64, lo: f64, hi: f64) -> f64 {
    if v.is_nan() {
        lo
    } else if v < lo {
        lo
    } else if v > hi {
        hi
    } else {
        v
    }
}

fn clamp_i32(v: i32, lo: i32, hi: i32) -> i32 {
    if v < lo {
        lo
    } else if v > hi {
        hi
    } else {
        v
    }
}

#[derive(Clone, Debug)]
struct Chat {
    id: String,
    title: String,
    pinned: bool,
}

#[derive(Clone, Debug)]
struct Settings {
    model: String,
    system_prompt: String,
    temperature: f64,
    top_p: f64,
    max_tokens: i32,
    zoom: i32,
    maximized: bool,
    window_width: i32,
    window_height: i32,
    provider: String,
    api_base: String,
    api_key: String,
    theme: String,
    presence_penalty: f64,
    frequency_penalty: f64,
    seed: i32,
    stop_sequences: String,
    top_k: i32,
    sidebar_collapsed: bool,
    accent_color: String,
    tavily_api_key: String,
    web_search_enabled: bool,
}

fn load_settings(conn: &Connection) -> Settings {
    load_settings_for(conn, current_user_id(conn))
}

fn load_settings_for(conn: &Connection, uid: i64) -> Settings {
    // Ensure a row exists for this user
    let _ = conn.execute(
        "INSERT OR IGNORE INTO user_settings (user_id) VALUES (?1)",
        params![uid],
    );
    conn.query_row(
        "SELECT model, system_prompt, temperature, top_p, max_tokens, zoom, maximized, window_width, window_height, provider, api_base, api_key, theme, presence_penalty, frequency_penalty, seed, stop_sequences, top_k, sidebar_collapsed, accent_color, tavily_api_key, web_search_enabled FROM user_settings WHERE user_id = ?1",
        params![uid],
        |row: &Row| {
            Ok(Settings {
                model: sanitize_field(&row.get::<_, String>(0)?, MAX_FIELD_LEN),
                system_prompt: sanitize_multiline(
                    &row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    MAX_SYSTEM_PROMPT_CHARS,
                ),
                temperature: clamp_f64(row.get::<_, Option<f64>>(2)?.unwrap_or(0.7), 0.0, 2.0),
                top_p: clamp_f64(row.get::<_, Option<f64>>(3)?.unwrap_or(0.95), 0.0, 1.0),
                max_tokens: clamp_i32(
                    clamp_to_i32(row.get::<_, Option<i64>>(4)?.unwrap_or(2048)),
                    1,
                    1_000_000,
                ),
                zoom: clamp_i32(
                    clamp_to_i32(row.get::<_, Option<i64>>(5)?.unwrap_or(115)),
                    50,
                    200,
                ),
                maximized: true,
                window_width: clamp_to_i32(row.get::<_, Option<i64>>(7)?.unwrap_or(1024)),
                window_height: clamp_to_i32(row.get::<_, Option<i64>>(8)?.unwrap_or(768)),
                provider: sanitize_field(
                    &row.get::<_, Option<String>>(9)?.unwrap_or_else(|| "ollama".to_string()),
                    64,
                ),
                api_base: sanitize_field(
                    &row.get::<_, Option<String>>(10)?
                        .unwrap_or_else(|| "http://localhost:11434".to_string()),
                    MAX_FIELD_LEN,
                ),
                api_key: sanitize_field(
                    &row.get::<_, Option<String>>(11)?.unwrap_or_default(),
                    MAX_FIELD_LEN,
                ),
                theme: {
                    let t = row.get::<_, Option<String>>(12)?.unwrap_or_else(|| "light".to_string());
                    if t == "dark" || t == "light" { t } else { "light".to_string() }
                },
                presence_penalty: clamp_f64(row.get::<_, f64>(13).unwrap_or(0.0), -2.0, 2.0),
                frequency_penalty: clamp_f64(row.get::<_, f64>(14).unwrap_or(0.0), -2.0, 2.0),
                seed: clamp_to_i32(row.get::<_, i64>(15).unwrap_or(-1)).max(-1),
                stop_sequences: sanitize_field(
                    &row.get::<_, String>(16).unwrap_or_default(),
                    MAX_STOP_FIELD_LEN,
                ),
                top_k: clamp_i32(clamp_to_i32(row.get::<_, i64>(17).unwrap_or(0)), 0, 1000),
                sidebar_collapsed: row.get::<_, i64>(18).unwrap_or(0) != 0,
                accent_color: parse_hex_color(
                    &row.get::<_, String>(19).unwrap_or_else(|_| DEFAULT_ACCENT.to_string()),
                )
                .unwrap_or_else(|| DEFAULT_ACCENT.to_string()),
                tavily_api_key: sanitize_field(
                    &row.get::<_, String>(20).unwrap_or_default(),
                    MAX_FIELD_LEN,
                ),
                web_search_enabled: row.get::<_, i64>(21).unwrap_or(0) != 0,
            })
        },
    )
    .unwrap()
}

fn save_settings(conn: &Connection, s: &Settings) {
    save_settings_for(conn, current_user_id(conn), s);
}

fn save_settings_for(conn: &Connection, uid: i64, s: &Settings) {
    let max_tokens: i64 = s.max_tokens.into();
    let zoom: i64 = s.zoom.into();
    let width: i64 = s.window_width.into();
    let height: i64 = s.window_height.into();

    let safe_theme = if s.theme == "dark" || s.theme == "light" {
        s.theme.clone()
    } else {
        "light".to_string()
    };
    let safe_accent = parse_hex_color(&s.accent_color).unwrap_or_else(|| DEFAULT_ACCENT.to_string());
    conn.execute(
        "INSERT OR REPLACE INTO user_settings (user_id, model, system_prompt, temperature, top_p, max_tokens, zoom, maximized, window_width, window_height, provider, api_base, api_key, theme, presence_penalty, frequency_penalty, seed, stop_sequences, top_k, sidebar_collapsed, accent_color, tavily_api_key, web_search_enabled) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23)",
        params![
            uid,
            sanitize_field(&s.model, MAX_FIELD_LEN),
            sanitize_multiline(&s.system_prompt, MAX_SYSTEM_PROMPT_CHARS),
            clamp_f64(s.temperature, 0.0, 2.0),
            clamp_f64(s.top_p, 0.0, 1.0),
            clamp_i32(clamp_to_i32(max_tokens), 1, 1_000_000),
            clamp_i32(clamp_to_i32(zoom), 50, 200),
            if s.maximized { 1 } else { 0 },
            clamp_to_i32(width),
            clamp_to_i32(height),
            sanitize_field(&s.provider, 64),
            sanitize_field(&s.api_base, MAX_FIELD_LEN),
            sanitize_field(&s.api_key, MAX_FIELD_LEN),
            safe_theme,
            clamp_f64(s.presence_penalty, -2.0, 2.0),
            clamp_f64(s.frequency_penalty, -2.0, 2.0),
            s.seed.max(-1),
            sanitize_field(&s.stop_sequences, MAX_STOP_FIELD_LEN),
            clamp_i32(s.top_k, 0, 1000),
            if s.sidebar_collapsed { 1 } else { 0 },
            safe_accent,
            sanitize_field(&s.tavily_api_key, MAX_FIELD_LEN),
            if s.web_search_enabled { 1 } else { 0 },
        ],
    )
    .unwrap();
}

fn enforce_history_limit(conn: &Connection, chat_id: &str) {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE chat_id = ?1",
            params![chat_id],
            |r| r.get(0),
        )
        .unwrap_or(0);

    if count <= MAX_HISTORY_MESSAGES {
        return;
    }

    if let Ok(cutoff_id) = conn.query_row(
        "SELECT id FROM messages WHERE chat_id = ?1 ORDER BY id DESC LIMIT 1 OFFSET ?2",
        params![chat_id, MAX_HISTORY_MESSAGES - 1],
        |r| r.get::<_, i64>(0),
    ) {
        let _ = conn.execute(
            "DELETE FROM messages WHERE chat_id = ?1 AND id <= ?2",
            params![chat_id, cutoff_id],
        );
    }
}

/* ================= PROVIDER PRESETS ================= */

// (id, label, default_host, model_hint)
const PROVIDER_PRESETS: &[(&str, &str, &str, &str)] = &[
    ("ollama",     "Ollama (local)",                "http://localhost:11434",                       ""),
    ("lmstudio",   "LM Studio (local)",             "http://localhost:1234",                        ""),
    ("openai",     "OpenAI",                        "https://api.openai.com",                       "gpt-4o-mini, gpt-4o, o1, o3-mini"),
    ("anthropic",  "Anthropic Claude",              "https://api.anthropic.com",                    "claude-opus-4-7, claude-sonnet-4-6, claude-haiku-4-5"),
    ("deepseek",   "DeepSeek",                      "https://api.deepseek.com",                     "deepseek-chat, deepseek-reasoner"),
    ("gemini",     "Google Gemini",                 "https://generativelanguage.googleapis.com",    "gemini-2.0-flash, gemini-1.5-pro"),
    ("xai",        "xAI Grok",                      "https://api.x.ai",                             "grok-4, grok-2-latest"),
    ("mistral",    "Mistral",                       "https://api.mistral.ai",                       "mistral-large-latest, codestral-latest"),
    ("moonshot",   "Moonshot Kimi",                 "https://api.moonshot.ai",                      "kimi-k2-0905-preview, moonshot-v1-128k, moonshot-v1-32k"),
    ("zhipu",      "Zhipu GLM",                     "https://open.bigmodel.cn",                     "glm-4.5, glm-4-plus, glm-4-air, glm-4-flashx"),
    ("qwen",       "Alibaba Qwen (DashScope)",      "https://dashscope.aliyuncs.com",               "qwen-max, qwen-plus, qwen-turbo, qwen3-32b"),
    ("yi",         "01.AI Yi",                      "https://api.lingyiwanwu.com",                  "yi-large, yi-large-turbo, yi-medium"),
    ("groq",       "Groq",                          "https://api.groq.com",                         "llama-3.3-70b-versatile, mixtral-8x7b-32768"),
    ("cerebras",   "Cerebras",                      "https://api.cerebras.ai",                      "llama3.1-70b, llama-3.3-70b"),
    ("perplexity", "Perplexity",                    "https://api.perplexity.ai",                    "sonar, sonar-pro, sonar-reasoning"),
    ("openrouter", "OpenRouter",                    "https://openrouter.ai",                        "anthropic/claude-sonnet-4.6, openai/gpt-4o, deepseek/deepseek-chat"),
    ("together",   "Together AI",                   "https://api.together.xyz",                     "meta-llama/Llama-3.3-70B-Instruct-Turbo"),
    ("fireworks",  "Fireworks AI",                  "https://api.fireworks.ai",                     "accounts/fireworks/models/llama-v3p3-70b-instruct"),
    ("deepinfra",  "DeepInfra",                     "https://api.deepinfra.com",                    "meta-llama/Llama-3.3-70B-Instruct, deepseek-ai/DeepSeek-V3"),
    ("nvidia",     "NVIDIA NIM",                    "https://integrate.api.nvidia.com",             "meta/llama-3.3-70b-instruct, nvidia/llama-3.1-nemotron-70b-instruct"),
    ("custom",     "Custom (OpenAI-compatible)",    "",                                             ""),
];

fn provider_default_base(provider: &str) -> &'static str {
    PROVIDER_PRESETS
        .iter()
        .find(|(id, ..)| *id == provider)
        .map(|(_, _, base, _)| *base)
        .unwrap_or("")
}

fn provider_label(provider: &str) -> &'static str {
    PROVIDER_PRESETS
        .iter()
        .find(|(id, ..)| *id == provider)
        .map(|(_, label, _, _)| *label)
        .unwrap_or("Custom (OpenAI-compatible)")
}

fn provider_model_hint(provider: &str) -> &'static str {
    PROVIDER_PRESETS
        .iter()
        .find(|(id, ..)| *id == provider)
        .map(|(_, _, _, hint)| *hint)
        .unwrap_or("")
}

fn is_openai_compatible(provider: &str) -> bool {
    provider != "ollama"
}

// Path appended to api_base for chat completions.
// api_base is just the host (e.g. "https://api.openai.com"), no path component.
fn provider_chat_path(provider: &str) -> &'static str {
    match provider {
        "ollama" => "/api/chat",
        "groq" => "/openai/v1/chat/completions",
        "fireworks" => "/inference/v1/chat/completions",
        "gemini" => "/v1beta/openai/chat/completions",
        "perplexity" => "/chat/completions",
        "deepinfra" => "/v1/openai/chat/completions",
        "zhipu" => "/api/paas/v4/chat/completions",
        "qwen" => "/compatible-mode/v1/chat/completions",
        "custom" => "/chat/completions", // user supplies host that already includes /v1 if needed
        _ => "/v1/chat/completions",
    }
}

fn provider_models_path(provider: &str) -> &'static str {
    match provider {
        "ollama" => "/api/tags",
        "groq" => "/openai/v1/models",
        "fireworks" => "/inference/v1/models",
        "gemini" => "/v1beta/openai/models",
        "perplexity" => "/models",
        "deepinfra" => "/v1/openai/models",
        "zhipu" => "/api/paas/v4/models",
        "qwen" => "/compatible-mode/v1/models",
        "custom" => "/models",
        _ => "/v1/models",
    }
}

/* ================= STREAMING CHUNK PARSERS ================= */

// Returns Some((delta, done)) when a complete record is parsed; None when more bytes are needed.
fn extract_ollama_chunk(buf: &mut String) -> Option<(String, bool)> {
    let pos = buf.find('\n')?;
    let line = buf[..pos].to_string();
    buf.drain(..=pos);
    let line = line.trim();
    if line.is_empty() {
        return Some((String::new(), false));
    }
    if let Ok(v) = serde_json::from_str::<Value>(line) {
        let content = v
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        let done = v.get("done").and_then(|d| d.as_bool()).unwrap_or(false);
        Some((content, done))
    } else {
        Some((String::new(), false))
    }
}

/// Parses one SSE event from the OpenAI-format stream buffer.
/// Returns (delta_obj, finish_reason, content_only_for_legacy).
/// content_only_for_legacy is the concatenated `delta.content` text (back-compat helper).
fn extract_openai_chunk(buf: &mut String) -> Option<(String, bool)> {
    // Legacy two-tuple form kept for callers that only want content.
    let (delta, finish) = extract_openai_event(buf)?;
    let content = delta
        .get("content")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string();
    let done = finish.as_deref() == Some("[DONE]") || finish.as_deref() == Some("stop");
    Some((content, done))
}

/// Returns one SSE event's parsed data.
/// `delta` is `choices[0].delta` (or Null), `finish_reason` is the string finish reason
/// from `choices[0].finish_reason` if present, or `Some("[DONE]")` for the terminal marker.
fn extract_openai_event(buf: &mut String) -> Option<(Value, Option<String>)> {
    let pos = buf.find("\n\n")?;
    let event = buf[..pos].to_string();
    buf.drain(..pos + 2);

    let mut delta = Value::Null;
    let mut finish: Option<String> = None;

    for line in event.lines() {
        let line = line.trim();
        let Some(data) = line.strip_prefix("data:").map(str::trim) else {
            continue;
        };
        if data == "[DONE]" {
            return Some((Value::Null, Some("[DONE]".to_string())));
        }
        if let Ok(v) = serde_json::from_str::<Value>(data) {
            if let Some(choice) = v
                .get("choices")
                .and_then(|c| c.as_array())
                .and_then(|a| a.first())
            {
                if let Some(d) = choice.get("delta") {
                    delta = d.clone();
                }
                if let Some(reason) = choice.get("finish_reason").and_then(|r| r.as_str()) {
                    if !reason.is_empty() {
                        finish = Some(reason.to_string());
                    }
                }
            }
        }
    }

    Some((delta, finish))
}

/* ================= SETTINGS MODAL ================= */

#[component]
fn SettingsModal(
    settings: Signal<Settings>,
    show_settings: Signal<bool>,
    chats: Signal<Vec<Chat>>,
    messages: Signal<Vec<(String, String)>>,
    current_chat_id: Signal<Option<String>>,
) -> Element {
    let mut local_provider = use_signal(|| settings().provider.clone());
    let mut local_api_base = use_signal(|| settings().api_base.clone());
    let mut local_api_key = use_signal(|| settings().api_key.clone());
    let mut local_model = use_signal(|| settings().model.clone());
    let mut local_system = use_signal(|| settings().system_prompt.clone());
    let mut local_temp = use_signal(|| settings().temperature);
    let mut local_top_p = use_signal(|| settings().top_p);
    let mut local_max_tokens = use_signal(|| settings().max_tokens);
    let mut local_presence = use_signal(|| settings().presence_penalty);
    let mut local_frequency = use_signal(|| settings().frequency_penalty);
    let mut local_seed = use_signal(|| settings().seed);
    let mut local_stop = use_signal(|| settings().stop_sequences.clone());
    let mut local_top_k = use_signal(|| settings().top_k);
    let mut local_accent = use_signal(|| settings().accent_color.clone());
    let mut local_tavily = use_signal(|| settings().tavily_api_key.clone());
    let mut local_zoom = use_signal(|| settings().zoom);
    let local_width = use_signal(|| settings().window_width);
    let local_height = use_signal(|| settings().window_height);
    let mut local_theme = use_signal(|| settings().theme.clone());

    let available_models = use_signal(|| Vec::<String>::new());
    let model_load_state = use_signal(|| String::from("idle")); // idle | loading | error

    // Refetch models whenever provider/base/key changes
    {
        let mut models_sig = available_models.clone();
        let mut state_sig = model_load_state.clone();
        let provider = local_provider.clone();
        let base = local_api_base.clone();
        let key = local_api_key.clone();
        use_effect(move || {
            let provider = provider();
            let base = base();
            let key = key();
            state_sig.set("loading".to_string());
            spawn(async move {
                let client = Client::builder()
                    .timeout(std::time::Duration::from_secs(8))
                    .build()
                    .unwrap_or_else(|_| Client::new());
                let names = if provider == "ollama" {
                    fetch_ollama_models(&client, &base).await
                } else {
                    fetch_openai_models(&client, &base, &key, &provider).await
                };
                match names {
                    Ok(n) => {
                        models_sig.set(n);
                        state_sig.set("idle".to_string());
                    }
                    Err(_) => {
                        models_sig.set(Vec::new());
                        state_sig.set("error".to_string());
                    }
                }
            });
        });
    }

    // Reset local fields whenever the modal becomes visible
    {
        let show_settings_sig = show_settings.clone();
        let settings_sig = settings.clone();
        let mut local_provider_sig = local_provider.clone();
        let mut local_api_base_sig = local_api_base.clone();
        let mut local_api_key_sig = local_api_key.clone();
        let mut local_model_sig = local_model.clone();
        let mut local_system_sig = local_system.clone();
        let mut local_temp_sig = local_temp.clone();
        let mut local_top_p_sig = local_top_p.clone();
        let mut local_max_tokens_sig = local_max_tokens.clone();
        let mut local_zoom_sig = local_zoom.clone();
        let mut local_width_sig = local_width.clone();
        let mut local_height_sig = local_height.clone();
        let mut local_theme_sig = local_theme.clone();
        let mut local_presence_sig = local_presence.clone();
        let mut local_frequency_sig = local_frequency.clone();
        let mut local_seed_sig = local_seed.clone();
        let mut local_stop_sig = local_stop.clone();
        let mut local_top_k_sig = local_top_k.clone();
        let mut local_accent_sig = local_accent.clone();
        let mut local_tavily_sig = local_tavily.clone();
        use_effect(move || {
            if show_settings_sig() {
                let s = settings_sig();
                local_provider_sig.set(s.provider.clone());
                local_api_base_sig.set(s.api_base.clone());
                local_api_key_sig.set(s.api_key.clone());
                local_model_sig.set(s.model.clone());
                local_system_sig.set(s.system_prompt.clone());
                local_temp_sig.set(s.temperature);
                local_top_p_sig.set(s.top_p);
                local_max_tokens_sig.set(s.max_tokens);
                local_zoom_sig.set(s.zoom);
                local_width_sig.set(s.window_width);
                local_height_sig.set(s.window_height);
                local_theme_sig.set(s.theme.clone());
                local_presence_sig.set(s.presence_penalty);
                local_frequency_sig.set(s.frequency_penalty);
                local_seed_sig.set(s.seed);
                local_stop_sig.set(s.stop_sequences.clone());
                local_top_k_sig.set(s.top_k);
                local_accent_sig.set(s.accent_color.clone());
                local_tavily_sig.set(s.tavily_api_key.clone());
            }
        });
    }

    let options_vec = {
        let mut v = available_models().clone();
        let selected = local_model().clone();
        if !selected.is_empty() && !v.iter().any(|s| s == &selected) {
            v.insert(0, selected);
        }
        v
    };

    let provider_options: Vec<&'static str> =
        PROVIDER_PRESETS.iter().map(|(id, ..)| *id).collect();

    let on_provider_change = {
        let mut local_provider = local_provider.clone();
        let mut local_api_base = local_api_base.clone();
        let mut local_model = local_model.clone();
        move |e: Event<FormData>| {
            let new_provider = e.value();
            let default_base = provider_default_base(&new_provider).to_string();
            local_api_base.set(default_base);
            local_provider.set(new_provider);
            local_model.set(String::new());
        }
    };

    let apply = {
        to_owned![
            local_provider,
            local_api_base,
            local_api_key,
            local_model,
            local_system,
            local_temp,
            local_top_p,
            local_max_tokens,
            local_zoom,
            local_width,
            local_height,
            local_theme,
            local_presence,
            local_frequency,
            local_seed,
            local_stop,
            local_top_k,
            local_accent,
            local_tavily,
            settings,
            show_settings
        ];
        move |_| {
            let prev = settings();
            let api_base_str = sanitize_field(local_api_base().trim_end_matches('/'), MAX_FIELD_LEN);
            let new_settings = Settings {
                model: sanitize_field(&local_model(), MAX_FIELD_LEN),
                system_prompt: sanitize_multiline(&local_system(), MAX_SYSTEM_PROMPT_CHARS),
                temperature: clamp_f64(local_temp(), 0.0, 2.0),
                top_p: clamp_f64(local_top_p(), 0.0, 1.0),
                max_tokens: clamp_i32(clamp_to_i32(local_max_tokens().into()), 1, 1_000_000),
                zoom: clamp_i32(clamp_to_i32(local_zoom().into()), 50, 200),
                maximized: true,
                window_width: clamp_to_i32(local_width().into()),
                window_height: clamp_to_i32(local_height().into()),
                provider: sanitize_field(&local_provider(), 64),
                api_base: api_base_str,
                api_key: sanitize_field(&local_api_key(), MAX_FIELD_LEN),
                theme: local_theme(),
                presence_penalty: clamp_f64(local_presence(), -2.0, 2.0),
                frequency_penalty: clamp_f64(local_frequency(), -2.0, 2.0),
                seed: local_seed().max(-1),
                stop_sequences: sanitize_field(&local_stop(), MAX_STOP_FIELD_LEN),
                top_k: clamp_i32(local_top_k(), 0, 1000),
                sidebar_collapsed: prev.sidebar_collapsed,
                accent_color: parse_hex_color(&local_accent()).unwrap_or_else(|| DEFAULT_ACCENT.to_string()),
                tavily_api_key: sanitize_field(&local_tavily(), MAX_FIELD_LEN),
                web_search_enabled: prev.web_search_enabled,
            };
            let conn = init_db();
            save_settings(&conn, &new_settings);
            settings.set(new_settings);
            show_settings.set(false);
        }
    };

    let delete_all = {
        to_owned![chats, messages, current_chat_id, show_settings];
        move |_| {
            let conn = init_db();
            let uid = current_user_id(&conn);
            conn.execute(
                "DELETE FROM messages WHERE chat_id IN (SELECT id FROM chats WHERE user_id = ?1)",
                params![uid],
            )
            .ok();
            conn.execute("DELETE FROM chats WHERE user_id = ?1", params![uid]).ok();
            chats.set(vec![]);
            messages.set(vec![]);
            current_chat_id.set(None);
            show_settings.set(false);
        }
    };

    let cancel = {
        to_owned![show_settings];
        move |_| {
            show_settings.set(false);
        }
    };

    let key_placeholder = if local_provider() == "ollama" {
        "Not required for Ollama"
    } else {
        "sk-..."
    };

    rsx! {
        div { class: "settings-overlay",
            div { class: "settings-modal",
                h3 { "Settings" }

                label { "Provider" }
                select {
                    class: "input",
                    value: "{local_provider}",
                    onchange: on_provider_change,
                    {provider_options.iter().map(|p| rsx!( option {
                        selected: (*p == local_provider()),
                        value: "{p}",
                        "{provider_label(p)}"
                    }))}
                }

                label { "API base URL" }
                input {
                    class: "input",
                    r#type: "text",
                    value: "{local_api_base}",
                    placeholder: if local_provider() == "custom" {
                        "https://your-proxy.example.com/v1".to_string()
                    } else {
                        provider_default_base(&local_provider()).to_string()
                    },
                    oninput: move |e| local_api_base.set(e.value()),
                }

                if is_openai_compatible(&local_provider()) {
                    label { "API key" }
                    input {
                        class: "input",
                        r#type: "password",
                        value: "{local_api_key}",
                        placeholder: "{key_placeholder}",
                        oninput: move |e| local_api_key.set(e.value()),
                    }
                }

                label { "Model" }
                {
                    let has_models = !options_vec.is_empty();
                    let hint = provider_model_hint(&local_provider());
                    let placeholder = if hint.is_empty() {
                        "model name".to_string()
                    } else {
                        format!("e.g. {}", hint)
                    };
                    if has_models {
                        rsx! {
                            select {
                                class: "input",
                                value: "{local_model}",
                                onchange: move |e| local_model.set(e.value()),
                                option { selected: local_model().is_empty(), value: "", "— Select a model —" }
                                {options_vec.iter().map(|m| rsx!( option { selected: m == &local_model(), value: "{m}", "{m}" } ))}
                            }
                            details { class: "model-manual-toggle",
                                summary { "Type a custom model name" }
                                input {
                                    class: "input",
                                    r#type: "text",
                                    value: "{local_model}",
                                    placeholder: "{placeholder}",
                                    oninput: move |e| local_model.set(e.value()),
                                }
                            }
                        }
                    } else {
                        rsx! {
                            input {
                                class: "input",
                                r#type: "text",
                                value: "{local_model}",
                                placeholder: "{placeholder}",
                                oninput: move |e| local_model.set(e.value()),
                            }
                        }
                    }
                }

                {
                    let state = model_load_state();
                    if state == "loading" {
                        rsx!( p { class: "dim-text hint-text", "Loading available models..." } )
                    } else if state == "error" {
                        rsx!( p { class: "dim-text warning-text", "Could not reach the API. Check the base URL and key." } )
                    } else if local_model().is_empty() {
                        rsx!( p { class: "dim-text warning-text", "No model selected — pick one to send messages." } )
                    } else {
                        rsx!( Fragment {} )
                    }
                }

                label { "Theme" }
                div { class: "theme-row",
                    button {
                        class: if local_theme() == "dark" { "theme-pill active" } else { "theme-pill" },
                        onclick: move |_| local_theme.set("dark".to_string()),
                        "Dark"
                    }
                    button {
                        class: if local_theme() == "light" { "theme-pill active" } else { "theme-pill" },
                        onclick: move |_| local_theme.set("light".to_string()),
                        "Light"
                    }
                }

                label { "System prompt (optional)" }
                textarea {
                    class: "textarea",
                    value: "{local_system}",
                    oninput: move |e| local_system.set(e.value()),
                }

                div { class: "settings-section-title", "Sampling" }

                label { "Temperature ", span { class: "label-hint", "(0.0 – 2.0)" } }
                input {
                    class: "input",
                    r#type: "number",
                    step: "0.05",
                    min: "0",
                    max: "2",
                    value: "{local_temp}",
                    oninput: move |e| {
                        let v = e.value().parse::<f64>().unwrap_or(0.7);
                        local_temp.set(clamp_f64(v, 0.0, 2.0));
                    }
                }

                label { "Top-p ", span { class: "label-hint", "(0.0 – 1.0)" } }
                input {
                    class: "input",
                    r#type: "number",
                    step: "0.01",
                    min: "0",
                    max: "1",
                    value: "{local_top_p}",
                    oninput: move |e| {
                        let v = e.value().parse::<f64>().unwrap_or(0.95);
                        local_top_p.set(clamp_f64(v, 0.0, 1.0));
                    }
                }

                label { "Top-k ", span { class: "label-hint", "(0 = disabled, max 1000)" } }
                input {
                    class: "input",
                    r#type: "number",
                    step: "1",
                    min: "0",
                    max: "1000",
                    value: "{local_top_k}",
                    oninput: move |e| {
                        let v = e.value().parse::<i32>().unwrap_or(0);
                        local_top_k.set(clamp_i32(v, 0, 1000));
                    }
                }

                label { "Max tokens ", span { class: "label-hint", "(1 – 1,000,000)" } }
                input {
                    class: "input",
                    r#type: "number",
                    step: "1",
                    min: "1",
                    max: "1000000",
                    value: "{local_max_tokens}",
                    oninput: move |e| {
                        let parsed = e.value().parse::<i64>().unwrap_or(2048);
                        local_max_tokens.set(clamp_i32(clamp_to_i32(parsed), 1, 1_000_000));
                    }
                }

                label { "Presence penalty ", span { class: "label-hint", "(-2.0 – 2.0)" } }
                input {
                    class: "input",
                    r#type: "number",
                    step: "0.05",
                    min: "-2",
                    max: "2",
                    value: "{local_presence}",
                    oninput: move |e| {
                        let v = e.value().parse::<f64>().unwrap_or(0.0);
                        local_presence.set(clamp_f64(v, -2.0, 2.0));
                    }
                }

                label { "Frequency penalty ", span { class: "label-hint", "(-2.0 – 2.0)" } }
                input {
                    class: "input",
                    r#type: "number",
                    step: "0.05",
                    min: "-2",
                    max: "2",
                    value: "{local_frequency}",
                    oninput: move |e| {
                        let v = e.value().parse::<f64>().unwrap_or(0.0);
                        local_frequency.set(clamp_f64(v, -2.0, 2.0));
                    }
                }

                label { "Seed ", span { class: "label-hint", "(-1 = random)" } }
                input {
                    class: "input",
                    r#type: "number",
                    step: "1",
                    min: "-1",
                    value: "{local_seed}",
                    oninput: move |e| {
                        let v = e.value().parse::<i32>().unwrap_or(-1);
                        local_seed.set(v.max(-1));
                    }
                }

                label { "Stop sequences ", span { class: "label-hint", "(comma-separated)" } }
                input {
                    class: "input",
                    r#type: "text",
                    value: "{local_stop}",
                    placeholder: "###, END",
                    oninput: move |e| local_stop.set(e.value()),
                }

                p { class: "dim-text hint-text",
                    "Some providers ignore parameters they don't support — that's fine, the call still succeeds."
                }

                div { class: "settings-section-title", "Tools" }

                label { "Tavily API key (for web search)" }
                input {
                    class: "input",
                    r#type: "password",
                    value: "{local_tavily}",
                    placeholder: "tvly-...",
                    oninput: move |e| local_tavily.set(sanitize_field(&e.value(), MAX_FIELD_LEN)),
                }
                p { class: "dim-text hint-text",
                    "Free tier at tavily.com gives 1,000 searches/month. The globe button in the chat input toggles it on/off per session."
                }

                div { class: "settings-section-title", "Display" }

                label { "Accent color" }
                div { class: "color-row",
                    input {
                        class: "color-swatch",
                        r#type: "color",
                        value: "{local_accent}",
                        oninput: move |e| {
                            if let Some(hex) = parse_hex_color(&e.value()) {
                                local_accent.set(hex);
                            }
                        },
                    }
                    input {
                        class: "input color-text",
                        r#type: "text",
                        value: "{local_accent}",
                        placeholder: "#ff7d3b",
                        oninput: move |e| {
                            // accept while typing; only valid hex is committed on save
                            local_accent.set(safe_truncate(&e.value(), 7));
                        },
                    }
                    button {
                        class: "color-reset",
                        onclick: move |_| local_accent.set(DEFAULT_ACCENT.to_string()),
                        "Reset"
                    }
                }

                label { "Zoom ", span { class: "label-hint", "(50 – 200%)" } }
                div { class: "zoom-row",
                    button { onclick: move |_| { local_zoom.set((local_zoom() - 10).max(50)); }, "−" }
                    span { "{local_zoom}%" }
                    button { onclick: move |_| { local_zoom.set((local_zoom() + 10).min(200)); }, "+" }
                }

                div { class: "modal-actions",
                    button { onclick: apply, class: "primary", "Apply" }
                    button { onclick: delete_all, class: "delete-all", "Delete All History" }
                    button { onclick: cancel, "Cancel" }
                }
            }
        }
    }
}

/* ================= APP ================= */

#[component]
fn App() -> Element {
    let conn = init_db();
    let initial_uid = current_user_id(&conn);

    let chats = use_signal(|| Vec::<Chat>::new());
    let current_chat_id = use_signal(|| Option::<String>::None);
    let messages = use_signal(|| Vec::<(String, String)>::new());

    // Provide current user via context so any descendant can read it without prop drilling.
    let current_user_sig = use_context_provider(|| Signal::new(initial_uid));
    // Global error popup queue (oldest first). Consumed by ErrorPopup component.
    let error_queue: Signal<Vec<ErrorEntry>> =
        use_context_provider(|| Signal::new(Vec::<ErrorEntry>::new()));

    let current_user = use_context_provider(|| {
        let conn = init_db();
        Signal::new(load_user(&conn, initial_uid))
    });
    let show_login = use_signal(|| false);
    let show_profile = use_signal(|| false);

    let settings = use_signal(|| load_settings_for(&conn, initial_uid));
    let show_settings = use_signal(|| false);

    // Reload chats whenever the active user changes
    {
        let mut chats = chats.clone();
        let mut current_chat_id = current_chat_id.clone();
        let mut messages = messages.clone();
        use_effect(move || {
            let uid = current_user_sig();
            let conn = init_db();
            let mut stmt = conn
                .prepare(
                    "SELECT id, title, COALESCE(pinned, 0) FROM chats WHERE user_id = ?1 ORDER BY pinned DESC, rowid",
                )
                .unwrap();
            let rows = stmt
                .query_map(params![uid], |row| {
                    Ok(Chat {
                        id: row.get::<_, String>(0)?,
                        title: row.get::<_, String>(1)?,
                        pinned: row.get::<_, i64>(2)? != 0,
                    })
                })
                .unwrap();
            chats.set(rows.map(|r| r.unwrap()).collect());
            current_chat_id.set(None);
            messages.set(vec![]);
        });
    }

    // Reload settings + user info when the active user changes
    {
        let mut settings = settings.clone();
        let mut current_user = current_user.clone();
        use_effect(move || {
            let uid = current_user_sig();
            let conn = init_db();
            settings.set(load_settings_for(&conn, uid));
            current_user.set(load_user(&conn, uid));
        });
    }
    let _ = error_queue;

    // Accent color is sanitized; if invalid, fall back to default
    let accent = parse_hex_color(&settings().accent_color)
        .unwrap_or_else(|| DEFAULT_ACCENT.to_string());
    // Slightly brighter "hover" accent: lighten by 8% via mix-percentage trick handled in CSS
    let container_style = format!(
        "width: 100vw; height: 100vh; --accent: {accent}; --accent-hover: {accent};"
    );
    let zoom_style = format!("zoom: {}%;", settings().zoom);
    let theme_class = format!("theme-{}", settings().theme);
    let sidebar_class = if settings().sidebar_collapsed {
        "app-container sidebar-collapsed"
    } else {
        "app-container"
    };

    let new_chat_action = {
        let mut chats = chats.clone();
        let mut current_chat_id = current_chat_id.clone();
        let mut messages = messages.clone();
        let current_user_sig = current_user_sig.clone();
        move || {
            let conn = init_db();
            let uid = current_user_sig();
            let new_id = Uuid::new_v4().to_string();
            let title = "New Chat".to_string();
            conn.execute(
                "INSERT INTO chats (id, title, pinned, user_id) VALUES (?1, ?2, 0, ?3)",
                params![new_id, title, uid],
            )
            .unwrap();
            chats.push(Chat {
                id: new_id.clone(),
                title,
                pinned: false,
            });
            current_chat_id.set(Some(new_id));
            messages.set(vec![]);
        }
    };

    let toggle_sidebar = {
        let mut settings = settings.clone();
        move || {
            let mut s = settings();
            s.sidebar_collapsed = !s.sidebar_collapsed;
            let conn = init_db();
            save_settings(&conn, &s);
            settings.set(s);
        }
    };

    let global_keydown = {
        let mut new_chat_action = new_chat_action.clone();
        let mut toggle_sidebar = toggle_sidebar.clone();
        let mut show_settings = show_settings.clone();
        move |e: Event<KeyboardData>| {
            let ctrl = e.modifiers().ctrl() || e.modifiers().meta();
            if !ctrl {
                if e.key() == Key::Escape && show_settings() {
                    show_settings.set(false);
                    e.prevent_default();
                }
                return;
            }
            match e.key() {
                Key::Character(ref c) if c.eq_ignore_ascii_case("n") => {
                    new_chat_action();
                    e.prevent_default();
                }
                Key::Character(ref c) if c.eq_ignore_ascii_case("b") => {
                    toggle_sidebar();
                    e.prevent_default();
                }
                Key::Character(ref c) if c == "," => {
                    show_settings.set(!show_settings());
                    e.prevent_default();
                }
                _ => {}
            }
        }
    };

    rsx! {
        document::Link { rel: "icon", href: FAVICON }
        document::Link { rel: "stylesheet", href: MAIN_CSS }

        div {
            class: "outer-wrapper {theme_class}",
            style: "{container_style}",
            tabindex: "0",
            onkeydown: global_keydown,

            div { class: "{sidebar_class}", style: "{zoom_style}",
                Sidebar {
                    chats: chats.clone(),
                    current_chat_id: current_chat_id.clone(),
                    messages: messages.clone(),
                    show_settings: show_settings.clone(),
                    show_login: show_login.clone(),
                    show_profile: show_profile.clone(),
                    current_user: current_user.clone(),
                }
                ChatWindow {
                    current_chat_id: current_chat_id.clone(),
                    messages: messages.clone(),
                    settings: settings.clone(),
                    chats: chats.clone()
                }
            }

            if show_settings() {
                SettingsModal {
                    settings: settings.clone(),
                    show_settings: show_settings.clone(),
                    chats: chats.clone(),
                    messages: messages.clone(),
                    current_chat_id: current_chat_id.clone()
                }
            }

            if show_login() {
                LoginModal { show_login: show_login.clone() }
            }

            if show_profile() {
                ProfileModal {
                    show_profile: show_profile.clone(),
                    current_user: current_user.clone(),
                }
            }

            ErrorPopup {}
        }
    }
}

/* ================= SIDEBAR ================= */

#[component]
fn RenameInputRow(
    initial: String,
    on_save: EventHandler<String>,
    on_cancel: EventHandler<()>,
) -> Element {
    let mut text = use_signal(|| initial);

    let do_save = move || {
        let t = sanitize_field(&text(), MAX_TITLE_LEN);
        on_save.call(t);
    };

    rsx! {
        div { class: "rename-row",
            input {
                class: "rename-input",
                autofocus: true,
                value: "{text}",
                onclick: move |e| e.stop_propagation(),
                oninput: move |e| {
                    text.set(sanitize_field(&e.value(), MAX_TITLE_LEN));
                },
                onkeydown: {
                    let mut do_save = do_save.clone();
                    move |e: Event<KeyboardData>| {
                        if e.key() == Key::Enter {
                            e.prevent_default();
                            do_save();
                        } else if e.key() == Key::Escape {
                            e.prevent_default();
                            on_cancel.call(());
                        }
                    }
                },
            }
            button {
                class: "rename-save",
                onclick: move |e| {
                    e.stop_propagation();
                    do_save();
                },
                "Save"
            }
            button {
                class: "rename-cancel",
                onclick: move |e| {
                    e.stop_propagation();
                    on_cancel.call(());
                },
                "Cancel"
            }
        }
    }
}

#[component]
fn Sidebar(
    chats: Signal<Vec<Chat>>,
    current_chat_id: Signal<Option<String>>,
    messages: Signal<Vec<(String, String)>>,
    show_settings: Signal<bool>,
    show_login: Signal<bool>,
    show_profile: Signal<bool>,
    current_user: Signal<User>,
) -> Element {
    let current_user_sig = use_context::<Signal<i64>>();
    let mut editing_chat = use_signal(|| Option::<String>::None);
    let mut open_menu = use_signal(|| Option::<String>::None);

    rsx! {
        div { class: "sidebar",
            div { class: "brand",
                img { class: "brand-icon", src: APP_ICON_SVG, alt: "" }
                h1 { class: "logo", "overlooked" }
            }

            button {
                class: "new-chat-btn big",
                onclick: move |_| {
                    let conn = init_db();
                    let uid = current_user_sig();
                    let new_id = Uuid::new_v4().to_string();
                    let title = "New Chat".to_string();

                    conn.execute(
                        "INSERT INTO chats (id, title, pinned, user_id) VALUES (?1, ?2, 0, ?3)",
                        params![new_id, title, uid],
                    ).unwrap();

                    chats.push(Chat { id: new_id.clone(), title, pinned: false });
                    current_chat_id.set(Some(new_id));
                    messages.set(vec![]);
                },
                span { class: "btn-icon", "+" }
                span { "New chat" }
            }

            div { class: "chat-list",
                {
                    let all = chats();
                    let total = all.len();
                    all.into_iter().enumerate().map(move |(idx, chat)| {
                    let id_owned = chat.id.clone();
                    let title_clone = chat.title.clone();
                    let pinned = chat.pinned;
                    // Open menu upward when this chat sits in the bottom 3 of the list
                    let dropup = total >= 4 && idx >= total - 3;

                    let id_for_open = id_owned.clone();
                    let id_for_save = id_owned.clone();
                    let id_for_rename = id_owned.clone();
                    let id_for_pin = id_owned.clone();
                    let id_for_delete = id_owned.clone();
                    let id_for_menu = id_owned.clone();

                    let mut chats_handle = chats.clone();
                    let mut messages_handle = messages.clone();
                    let mut current_chat_handle = current_chat_id.clone();
                    let mut editing_chat_handle = editing_chat.clone();
                    let mut open_menu_handle = open_menu.clone();

                    let is_active = current_chat_id().as_ref().map(|c| c == &id_owned).unwrap_or(false);
                    let mut classes = String::from("chat-item");
                    if is_active { classes.push_str(" active"); }
                    if pinned { classes.push_str(" pinned"); }
                    let menu_open = open_menu().as_ref().map(|m| m == &id_owned).unwrap_or(false);

                    rsx! {
                        div { class: "chat-item-row",
                            div {
                                class: "{classes}",
                                onclick: move |_| {
                                    let conn = init_db();
                                    let mut stmt = conn.prepare(
                                        "SELECT role, content FROM messages
                                         WHERE chat_id = ? ORDER BY id DESC LIMIT ?"
                                    ).unwrap();

                                    let rows = stmt
                                        .query_map(params![&id_for_open, MAX_HISTORY_MESSAGES], |row| {
                                            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                                        })
                                        .unwrap();

                                    let mut collected: Vec<(String, String)> = rows.map(|r| r.unwrap()).collect();
                                    collected.reverse();
                                    messages_handle.set(collected);
                                    current_chat_handle.set(Some(id_for_open.clone()));
                                },

                                {
                                    if editing_chat_handle().as_ref().map(|c| c == &id_for_save).unwrap_or(false) {
                                        let save_id = id_for_save.clone();
                                        let mut chats_for_save = chats_handle.clone();
                                        let mut editing_for_save = editing_chat_handle.clone();
                                        let mut editing_for_cancel = editing_chat_handle.clone();
                                        rsx! {
                                            RenameInputRow {
                                                initial: title_clone.clone(),
                                                on_save: move |new_title: String| {
                                                    let t = sanitize_field(&new_title, MAX_TITLE_LEN);
                                                    let conn = init_db();
                                                    conn.execute(
                                                        "UPDATE chats SET title = ?1 WHERE id = ?2",
                                                        params![t, save_id.clone()],
                                                    ).unwrap();
                                                    chats_for_save.set(
                                                        chats_for_save().into_iter().map(|c| {
                                                            if c.id == save_id {
                                                                Chat { id: c.id, title: t.clone(), pinned: c.pinned }
                                                            } else { c }
                                                        }).collect()
                                                    );
                                                    editing_for_save.set(None);
                                                },
                                                on_cancel: move |_| {
                                                    editing_for_cancel.set(None);
                                                },
                                            }
                                        }
                                    } else {
                                        rsx! {
                                            Fragment {
                                                if pinned {
                                                    span { class: "pin-mark", title: "Pinned",
                                                        svg {
                                                            view_box: "0 0 24 24", width: "12", height: "12",
                                                            fill: "currentColor",
                                                            path { d: "M16 12V4h1V2H7v2h1v8l-2 2v2h5.2v6h1.6v-6H18v-2l-2-2z" }
                                                        }
                                                    }
                                                }
                                                div { class: "chat-title", "{title_clone}" }
                                                div { class: "chat-menu-anchor",
                                                    button {
                                                        class: if menu_open { "menu-btn open" } else { "menu-btn" },
                                                        title: "More",
                                                        onclick: move |e| {
                                                            e.stop_propagation();
                                                            let cur = open_menu_handle();
                                                            if cur.as_ref().map(|m| m == &id_for_menu).unwrap_or(false) {
                                                                open_menu_handle.set(None);
                                                            } else {
                                                                open_menu_handle.set(Some(id_for_menu.clone()));
                                                            }
                                                        },
                                                        svg {
                                                            view_box: "0 0 24 24", width: "16", height: "16",
                                                            fill: "currentColor",
                                                            circle { cx: "5", cy: "12", r: "1.6" }
                                                            circle { cx: "12", cy: "12", r: "1.6" }
                                                            circle { cx: "19", cy: "12", r: "1.6" }
                                                        }
                                                    }
                                                    if menu_open {
                                                        div {
                                                            class: if dropup { "chat-menu dropup" } else { "chat-menu" },
                                                            onclick: move |e| e.stop_propagation(),
                                                            button {
                                                                class: "menu-item",
                                                                onclick: move |e| {
                                                                    e.stop_propagation();
                                                                    open_menu.set(None);
                                                                    editing_chat.set(Some(id_for_rename.clone()));
                                                                },
                                                                span { class: "menu-icon",
                                                                    svg {
                                                                        view_box: "0 0 24 24", width: "16", height: "16",
                                                                        fill: "none", stroke: "currentColor", stroke_width: "2",
                                                                        stroke_linecap: "round", stroke_linejoin: "round",
                                                                        path { d: "M12 20h9" }
                                                                        path { d: "M16.5 3.5a2.121 2.121 0 0 1 3 3L7 19l-4 1 1-4 12.5-12.5z" }
                                                                    }
                                                                }
                                                                span { "Rename" }
                                                            }
                                                            button {
                                                                class: "menu-item",
                                                                onclick: move |e| {
                                                                    e.stop_propagation();
                                                                    open_menu.set(None);
                                                                    let new_pinned = !pinned;
                                                                    let conn = init_db();
                                                                    conn.execute(
                                                                        "UPDATE chats SET pinned = ?1 WHERE id = ?2",
                                                                        params![if new_pinned { 1 } else { 0 }, id_for_pin.clone()],
                                                                    ).ok();

                                                                    let mut updated: Vec<Chat> = chats_handle().into_iter().map(|c| {
                                                                        if c.id == id_for_pin {
                                                                            Chat { id: c.id, title: c.title, pinned: new_pinned }
                                                                        } else { c }
                                                                    }).collect();
                                                                    updated.sort_by(|a, b| b.pinned.cmp(&a.pinned));
                                                                    chats_handle.set(updated);
                                                                },
                                                                span { class: "menu-icon",
                                                                    svg {
                                                                        view_box: "0 0 24 24", width: "16", height: "16",
                                                                        fill: "currentColor",
                                                                        path { d: "M16 12V4h1V2H7v2h1v8l-2 2v2h5.2v6h1.6v-6H18v-2l-2-2z" }
                                                                    }
                                                                }
                                                                span { if pinned { "Unpin" } else { "Pin" } }
                                                            }
                                                            div { class: "menu-sep" }
                                                            button {
                                                                class: "menu-item danger",
                                                                onclick: move |e| {
                                                                    e.stop_propagation();
                                                                    open_menu.set(None);
                                                                    let conn = init_db();

                                                                    conn.execute(
                                                                        "DELETE FROM messages WHERE chat_id = ?1",
                                                                        params![id_for_delete.clone()],
                                                                    ).unwrap();

                                                                    conn.execute(
                                                                        "DELETE FROM chats WHERE id = ?1",
                                                                        params![id_for_delete.clone()],
                                                                    ).unwrap();

                                                                    chats_handle.set(
                                                                        chats_handle()
                                                                            .into_iter()
                                                                            .filter(|c| c.id != id_for_delete)
                                                                            .collect()
                                                                    );

                                                                    if current_chat_handle() == Some(id_for_delete.clone()) {
                                                                        current_chat_handle.set(None);
                                                                        messages_handle.set(vec![]);
                                                                    }
                                                                },
                                                                span { class: "menu-icon",
                                                                    svg {
                                                                        view_box: "0 0 24 24", width: "16", height: "16",
                                                                        fill: "none", stroke: "currentColor", stroke_width: "2",
                                                                        stroke_linecap: "round", stroke_linejoin: "round",
                                                                        polyline { points: "3 6 5 6 21 6" }
                                                                        path { d: "M19 6l-2 14a2 2 0 0 1-2 2H9a2 2 0 0 1-2-2L5 6" }
                                                                        path { d: "M10 11v6" }
                                                                        path { d: "M14 11v6" }
                                                                        path { d: "M9 6V4a2 2 0 0 1 2-2h2a2 2 0 0 1 2 2v2" }
                                                                    }
                                                                }
                                                                span { "Delete" }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                })
                }
            }

            // Click-outside catcher for the open menu
            if open_menu().is_some() {
                div {
                    class: "menu-catcher",
                    onclick: move |_| open_menu.set(None),
                }
            }

            div { class: "sidebar-footer",
                {
                    let user = current_user();
                    let avatar = user.avatar_data.clone();
                    let initial = initial_for(&user.username);
                    let bg = color_for(&user.username);
                    let label = if user.is_guest { "Guest".to_string() } else { user.username.clone() };
                    let is_guest = user.is_guest;
                    rsx! {
                        button {
                            class: "user-pill",
                            title: if is_guest { "Sign in or create account" } else { "Profile" },
                            onclick: move |_| {
                                if is_guest {
                                    show_login.set(true);
                                } else {
                                    show_profile.set(true);
                                }
                            },
                            {
                                if let Some(data) = avatar {
                                    rsx! { img { class: "avatar", src: "{data}", alt: "" } }
                                } else {
                                    rsx! {
                                        span { class: "avatar avatar-initial",
                                            style: "background: {bg}",
                                            "{initial}"
                                        }
                                    }
                                }
                            }
                            span { class: "user-pill-name", "{label}" }
                            if is_guest {
                                span { class: "user-pill-action", "Sign in" }
                            }
                        }
                    }
                }
                button {
                    class: "footer-btn",
                    onclick: move |_| {
                        show_settings.set(!show_settings());
                    },
                    title: "Settings",
                    svg {
                        view_box: "0 0 24 24", width: "20", height: "20",
                        fill: "none", stroke: "currentColor", stroke_width: "2",
                        stroke_linecap: "round", stroke_linejoin: "round",
                        circle { cx: "12", cy: "12", r: "3" }
                        path { d: "M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1 0 2.83 2 2 0 0 1-2.83 0l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-2 2 2 2 0 0 1-2-2v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83 0 2 2 0 0 1 0-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1-2-2 2 2 0 0 1 2-2h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 0-2.83 2 2 0 0 1 2.83 0l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 2-2 2 2 0 0 1 2 2v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 0 2 2 0 0 1 0 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 2 2 2 2 0 0 1-2 2h-.09a1.65 1.65 0 0 0-1.51 1z" }
                    }
                    span { "Settings" }
                }
                a {
                    class: "footer-btn",
                    href: "https://github.com/KPCOFGS/overlooked",
                    target: "_blank",
                    title: "GitHub repository",
                    svg {
                        view_box: "0 0 24 24", width: "20", height: "20",
                        fill: "currentColor",
                        path { d: "M12 .5C5.65.5.5 5.65.5 12c0 5.08 3.29 9.39 7.86 10.92.58.11.79-.25.79-.55v-2.07c-3.2.7-3.87-1.36-3.87-1.36-.52-1.32-1.27-1.67-1.27-1.67-1.04-.71.08-.7.08-.7 1.15.08 1.76 1.18 1.76 1.18 1.02 1.75 2.68 1.24 3.34.95.1-.74.4-1.24.72-1.53-2.55-.29-5.24-1.28-5.24-5.69 0-1.26.45-2.29 1.18-3.1-.12-.29-.51-1.46.11-3.04 0 0 .97-.31 3.18 1.18a11.05 11.05 0 0 1 5.79 0c2.21-1.49 3.18-1.18 3.18-1.18.62 1.58.23 2.75.11 3.04.74.81 1.18 1.84 1.18 3.1 0 4.42-2.69 5.4-5.25 5.68.41.36.78 1.06.78 2.13v3.16c0 .31.21.67.8.55C20.21 21.39 23.5 17.08 23.5 12 23.5 5.65 18.35.5 12 .5z" }
                    }
                    span { "GitHub" }
                }
            }
        }
    }
}

/* ================= API STRUCTURES ================= */

#[derive(Serialize, Deserialize, Debug, Clone)]
struct ChatTurn {
    role: String,
    content: String,
}

/* ================= MODEL FETCHING ================= */

async fn fetch_ollama_models(client: &Client, base: &str) -> Result<Vec<String>, String> {
    let url = format!(
        "{}{}",
        base.trim_end_matches('/'),
        provider_models_path("ollama")
    );
    let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
    let json: Value = resp.json().await.map_err(|e| e.to_string())?;
    let mut names = Vec::new();
    if let Some(arr) = json.get("models").and_then(|v| v.as_array()) {
        for item in arr {
            if let Some(m) = item
                .get("model")
                .or(item.get("name"))
                .and_then(|v| v.as_str())
            {
                names.push(m.to_string());
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    names.retain(|n| seen.insert(n.clone()));
    Ok(names)
}

async fn fetch_openai_models(
    client: &Client,
    base: &str,
    key: &str,
    provider: &str,
) -> Result<Vec<String>, String> {
    let url = format!(
        "{}{}",
        base.trim_end_matches('/'),
        provider_models_path(provider)
    );
    let mut req = client.get(&url);
    if !key.is_empty() {
        req = req.bearer_auth(key);
    }
    let resp = req.send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let json: Value = resp.json().await.map_err(|e| e.to_string())?;
    let mut names = Vec::new();
    if let Some(arr) = json.get("data").and_then(|v| v.as_array()) {
        for item in arr {
            if let Some(id) = item.get("id").and_then(|v| v.as_str()) {
                names.push(id.to_string());
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    names.retain(|n| seen.insert(n.clone()));
    Ok(names)
}

/* ================= WEB SEARCH (TAVILY) ================= */

fn web_search_tool_def() -> Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": "web_search",
            "description": "Search the public web for up-to-date information. Use this whenever the user asks about recent events, current data, prices, news, or anything you may not know.",
            "parameters": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "A natural-language search query, like you'd type into Google."
                    }
                },
                "required": ["query"]
            }
        }
    })
}

async fn tavily_search(client: &Client, key: &str, query: &str) -> Result<String, String> {
    if key.trim().is_empty() {
        return Err("No Tavily API key configured. Open Settings to add one.".into());
    }
    let q = safe_truncate(query.trim(), MAX_SEARCH_QUERY_CHARS);
    if q.is_empty() {
        return Err("Empty search query.".into());
    }
    let body = serde_json::json!({
        "api_key": key,
        "query": q,
        "search_depth": "basic",
        "max_results": 5,
        "include_answer": true,
    });
    let resp = client
        .post("https://api.tavily.com/search")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Tavily request failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("Tavily HTTP {}", resp.status()));
    }
    let v: Value = resp
        .json()
        .await
        .map_err(|e| format!("Tavily parse failed: {e}"))?;

    let mut out = String::new();
    if let Some(answer) = v.get("answer").and_then(|s| s.as_str()) {
        out.push_str("Quick answer: ");
        out.push_str(answer);
        out.push_str("\n\n");
    }
    if let Some(results) = v.get("results").and_then(|r| r.as_array()) {
        out.push_str("Sources:\n");
        for r in results.iter().take(5) {
            let title = r.get("title").and_then(|s| s.as_str()).unwrap_or("");
            let url = r.get("url").and_then(|s| s.as_str()).unwrap_or("");
            let content = r.get("content").and_then(|s| s.as_str()).unwrap_or("");
            let snippet = safe_truncate(content, 600);
            out.push_str(&format!("- {title}\n  {url}\n  {snippet}\n\n"));
        }
    }
    if out.is_empty() {
        out.push_str("No results.");
    }
    Ok(safe_truncate(&out, 8000))
}

async fn execute_tool_call(
    client: &Client,
    settings: &Settings,
    name: &str,
    args_json: &str,
) -> String {
    if name == "web_search" {
        let args: Value = serde_json::from_str(args_json).unwrap_or(Value::Null);
        let query = args
            .get("query")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        match tavily_search(client, &settings.tavily_api_key, &query).await {
            Ok(s) => s,
            Err(e) => format!("Web search error: {e}"),
        }
    } else {
        format!("Unknown tool: {name}")
    }
}

/* ================= LOGIN / PROFILE / ERROR POPUP ================= */

// (id, expires_at_unix_secs, message)
type ErrorEntry = (u64, f64, String);

static NEXT_ERROR_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
const ERROR_TTL_SECS: f64 = 6.0;

fn now_seconds() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn push_error(queue: Signal<Vec<ErrorEntry>>, msg: impl Into<String>) {
    let id = NEXT_ERROR_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let expires = now_seconds() + ERROR_TTL_SECS;
    let mut q = queue.clone();
    q.write().push((id, expires, msg.into()));
}

#[component]
fn ErrorPopup() -> Element {
    let mut queue = use_context::<Signal<Vec<ErrorEntry>>>();

    // Polling pruner: every 500ms, drop expired entries.
    use_future(move || async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let now = now_seconds();
            let needs_prune = queue.read().iter().any(|(_, exp, _)| *exp <= now);
            if needs_prune {
                queue.write().retain(|(_, exp, _)| *exp > now);
            }
        }
    });

    let q = queue();
    if q.is_empty() {
        return rsx!(Fragment {});
    }
    let entries: Vec<ErrorEntry> = q.iter().rev().take(4).cloned().collect();
    rsx! {
        div { class: "error-toast-stack",
            {entries.iter().map(|(id, _, msg)| {
                let id = *id;
                let msg = msg.clone();
                rsx! {
                    div { class: "error-toast", key: "{id}",
                        span { class: "error-icon",
                            svg {
                                view_box: "0 0 24 24", width: "18", height: "18",
                                fill: "none", stroke: "currentColor", stroke_width: "2",
                                stroke_linecap: "round", stroke_linejoin: "round",
                                circle { cx: "12", cy: "12", r: "10" }
                                path { d: "M12 8v4" }
                                path { d: "M12 16h.01" }
                            }
                        }
                        span { class: "error-text", "{msg}" }
                        button {
                            class: "error-dismiss",
                            onclick: move |_| {
                                queue.write().retain(|(eid, _, _)| *eid != id);
                            },
                            "×"
                        }
                    }
                }
            })}
        }
    }
}

#[component]
fn LoginModal(show_login: Signal<bool>) -> Element {
    let mut current_user_sig = use_context::<Signal<i64>>();
    let queue = use_context::<Signal<Vec<ErrorEntry>>>();
    let mut tab = use_signal(|| "signin".to_string()); // "signin" | "signup"
    let mut username = use_signal(String::new);
    let mut password = use_signal(String::new);
    let mut password2 = use_signal(String::new);
    let mut local_error = use_signal(|| Option::<String>::None);

    let do_signin = {
        let mut show_login = show_login.clone();
        let mut local_error = local_error.clone();
        let queue = queue.clone();
        move |_| {
            let conn = init_db();
            match login_user(&conn, &username(), &password()) {
                Ok(uid) => {
                    set_current_user_id(&conn, uid);
                    current_user_sig.set(uid);
                    show_login.set(false);
                    local_error.set(None);
                }
                Err(e) => {
                    local_error.set(Some(e.clone()));
                    push_error(queue.clone(), e);
                }
            }
        }
    };

    let do_signup = {
        let mut show_login = show_login.clone();
        let mut local_error = local_error.clone();
        let queue = queue.clone();
        move |_| {
            if password() != password2() {
                local_error.set(Some("Passwords don't match.".into()));
                return;
            }
            let conn = init_db();
            match create_user(&conn, &username(), &password()) {
                Ok(uid) => {
                    set_current_user_id(&conn, uid);
                    current_user_sig.set(uid);
                    show_login.set(false);
                    local_error.set(None);
                }
                Err(e) => {
                    local_error.set(Some(e.clone()));
                    push_error(queue.clone(), e);
                }
            }
        }
    };

    let do_guest = {
        let mut show_login = show_login.clone();
        move |_| {
            let conn = init_db();
            set_current_user_id(&conn, 1);
            current_user_sig.set(1);
            show_login.set(false);
        }
    };

    rsx! {
        div { class: "settings-overlay",
            div { class: "settings-modal login-modal",
                h3 { "Welcome back" }
                p { class: "dim-text hint-text",
                    "Sign in to keep your chats and settings separate, or continue as a guest."
                }

                div { class: "tab-row",
                    button {
                        class: if tab() == "signin" { "tab active" } else { "tab" },
                        onclick: move |_| { tab.set("signin".into()); local_error.set(None); },
                        "Sign in"
                    }
                    button {
                        class: if tab() == "signup" { "tab active" } else { "tab" },
                        onclick: move |_| { tab.set("signup".into()); local_error.set(None); },
                        "Create account"
                    }
                }

                label { "Username" }
                input {
                    class: "input",
                    r#type: "text",
                    maxlength: "32",
                    autocomplete: "username",
                    placeholder: "3–32 letters, digits, _, -",
                    value: "{username}",
                    oninput: move |e| username.set(safe_truncate(&e.value(), 32)),
                }

                label { "Password" }
                input {
                    class: "input",
                    r#type: "password",
                    maxlength: "128",
                    autocomplete: if tab() == "signup" { "new-password" } else { "current-password" },
                    placeholder: "at least 8 characters",
                    value: "{password}",
                    oninput: move |e| password.set(safe_truncate(&e.value(), 128)),
                }

                if tab() == "signup" {
                    p { class: "dim-text hint-text",
                        "8–128 characters, must include at least one letter and one digit."
                    }
                    label { "Confirm password" }
                    input {
                        class: "input",
                        r#type: "password",
                        maxlength: "128",
                        autocomplete: "new-password",
                        value: "{password2}",
                        oninput: move |e| password2.set(safe_truncate(&e.value(), 128)),
                    }
                }

                if let Some(err) = local_error() {
                    p { class: "warning-text", "{err}" }
                }

                div { class: "modal-actions",
                    button {
                        onclick: do_guest,
                        class: "delete-all",
                        "Continue as guest"
                    }
                    button {
                        onclick: move |_| show_login.set(false),
                        "Cancel"
                    }
                    if tab() == "signin" {
                        button {
                            onclick: do_signin,
                            class: "primary",
                            "Sign in"
                        }
                    } else {
                        button {
                            onclick: do_signup,
                            class: "primary",
                            "Create"
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn ProfileModal(show_profile: Signal<bool>, current_user: Signal<User>) -> Element {
    let mut current_user_sig = use_context::<Signal<i64>>();
    let queue = use_context::<Signal<Vec<ErrorEntry>>>();
    let initial_user = current_user();
    let mut local_avatar = use_signal(|| initial_user.avatar_data.clone().unwrap_or_default());

    let mut current_user_for_save = current_user.clone();
    let do_save_avatar = move |_| {
        let conn = init_db();
        let val = local_avatar();
        let opt = if val.is_empty() { None } else { Some(val) };
        match update_avatar(&conn, current_user_sig(), opt.clone()) {
            Ok(()) => {
                current_user_for_save.set(load_user(&conn, current_user_sig()));
            }
            Err(e) => push_error(queue.clone(), e),
        }
    };

    let do_logout = {
        let mut show_profile = show_profile.clone();
        move |_| {
            let conn = init_db();
            set_current_user_id(&conn, 1);
            current_user_sig.set(1);
            show_profile.set(false);
        }
    };

    let avatar_preview = {
        let current = local_avatar();
        if current.is_empty() {
            let bg = color_for(&initial_user.username);
            let initial = initial_for(&initial_user.username);
            rsx! {
                div { class: "avatar avatar-large avatar-initial",
                    style: "background: {bg}",
                    "{initial}"
                }
            }
        } else {
            rsx! {
                img { class: "avatar avatar-large", src: "{current}", alt: "" }
            }
        }
    };

    rsx! {
        div { class: "settings-overlay",
            div { class: "settings-modal",
                h3 { "Profile" }
                p { class: "dim-text hint-text",
                    "Signed in as ", strong { "{initial_user.username}" }
                }

                label { "Avatar" }
                div { class: "avatar-row",
                    {avatar_preview}
                    div { class: "avatar-controls",
                        div { class: "row",
                            button {
                                class: "primary",
                                onclick: {
                                    let mut local_avatar = local_avatar.clone();
                                    let queue = queue.clone();
                                    move |_| {
                                        let picked = rfd::FileDialog::new()
                                            .add_filter("Image", &["png", "jpg", "jpeg", "webp", "gif"])
                                            .set_title("Choose avatar image")
                                            .pick_file();
                                        if let Some(path) = picked {
                                            match load_avatar_from_path(&path) {
                                                Ok(data_url) => local_avatar.set(data_url),
                                                Err(e) => push_error(queue.clone(), e),
                                            }
                                        }
                                    }
                                },
                                "Choose file..."
                            }
                            button {
                                onclick: move |_| local_avatar.set(String::new()),
                                "Clear"
                            }
                            button { onclick: do_save_avatar, class: "primary", "Save" }
                        }
                        p { class: "dim-text hint-text",
                            "PNG, JPEG, WEBP or GIF up to 1 MB. Stored locally in chat.db."
                        }
                        details {
                            summary { class: "advanced-summary", "Advanced: paste data URL" }
                            input {
                                class: "input",
                                r#type: "text",
                                placeholder: "data:image/png;base64,...",
                                value: "{local_avatar}",
                                oninput: move |e| local_avatar.set(safe_truncate(&e.value(), MAX_AVATAR_DATA_URL_LEN)),
                            }
                        }
                    }
                }

                div { class: "modal-actions",
                    button { onclick: do_logout, class: "delete-all", "Log out" }
                    button { onclick: move |_| show_profile.set(false), "Close" }
                }
            }
        }
    }
}

/* ================= MESSAGE COMPOSER ================= */

/// Owns its own text/clear state so fast typing can't be raced by parent re-renders.
/// The textarea is uncontrolled (no `value` attribute pushed from Rust); the DOM keeps
/// the cursor exactly where the user put it. We track the typed text via `oninput`
/// for the send-button-enable check, and clear via a `key`-based remount on send.
#[component]
fn MessageComposer(
    is_loading: bool,
    disabled_external: bool,
    settings: Signal<Settings>,
    on_send: EventHandler<String>,
    on_stop: EventHandler<()>,
) -> Element {
    let mut text = use_signal(|| String::new());
    let mut clear_epoch = use_signal(|| 0u64);

    let mut do_send = move || {
        let v = text();
        if v.trim().is_empty() || is_loading || disabled_external {
            return;
        }
        text.set(String::new());
        clear_epoch.set(clear_epoch() + 1);
        // Imperatively clear the DOM textarea. The textarea is uncontrolled
        // (no `value` attribute pushed back from Rust) so fast typing can't
        // race re-renders, but that means we have to clear it ourselves on send.
        let _ = document::eval(
            "(function(){var t=document.querySelector('.chat-input');if(t){t.value='';t.style.height='auto';}})();",
        );
        on_send.call(v);
    };

    let send_disabled = text().trim().is_empty() || is_loading || disabled_external;

    rsx! {
        div { class: "chat-input-area",
            div { class: "chat-input-pill",
                textarea {
                    key: "{clear_epoch}",
                    class: "chat-input",
                    placeholder: "Message...",
                    rows: "1",
                    maxlength: "{MAX_MESSAGE_CHARS}",
                    oninput: move |e| text.set(e.value()),
                    onkeydown: {
                        let mut do_send = do_send.clone();
                        move |e: Event<KeyboardData>| {
                            if e.key() == Key::Enter && !e.modifiers().shift() {
                                if !is_loading && !disabled_external {
                                    let v = text();
                                    if !v.trim().is_empty() {
                                        e.prevent_default();
                                        do_send();
                                    }
                                }
                            }
                        }
                    },
                    disabled: is_loading,
                }

                div { class: "chat-input-actions",
                    {
                        let s = settings();
                        let supported = is_openai_compatible(&s.provider);
                        let has_key = !s.tavily_api_key.trim().is_empty();
                        let on = s.web_search_enabled;
                        let title_text = if !supported {
                            format!("Web search needs an OpenAI-compatible provider (current: {})", provider_label(&s.provider))
                        } else if !has_key {
                            "Add a Tavily API key in Settings to use web search".to_string()
                        } else if on {
                            "Web search: on (click to disable)".to_string()
                        } else {
                            "Web search: off (click to enable)".to_string()
                        };
                        let mut classes = String::from("icon-circle-btn web-toggle");
                        if on { classes.push_str(" active"); }
                        let queue = use_context::<Signal<Vec<ErrorEntry>>>();
                        rsx! {
                            button {
                                class: "{classes}",
                                title: "{title_text}",
                                onclick: move |_| {
                                    if !supported {
                                        push_error(queue.clone(), format!("Web search isn't supported for the {} provider. Switch to an OpenAI-compatible provider in Settings.", provider_label(&settings().provider)));
                                        return;
                                    }
                                    if !has_key {
                                        push_error(queue.clone(), "No Tavily API key set. Open Settings → Tools and paste your key (free tier at tavily.com).");
                                        // still allow toggling so the user can pre-arm it
                                    }
                                    let mut s2 = settings();
                                    s2.web_search_enabled = !s2.web_search_enabled;
                                    let conn = init_db();
                                    save_settings(&conn, &s2);
                                    settings.set(s2);
                                },
                                svg {
                                    view_box: "0 0 24 24", width: "20", height: "20",
                                    fill: "none", stroke: "currentColor", stroke_width: "2",
                                    stroke_linecap: "round", stroke_linejoin: "round",
                                    circle { cx: "12", cy: "12", r: "10" }
                                    path { d: "M2 12h20" }
                                    path { d: "M12 2a15 15 0 0 1 0 20" }
                                    path { d: "M12 2a15 15 0 0 0 0 20" }
                                }
                            }
                        }
                    }

                    { if is_loading {
                        rsx! {
                            button {
                                class: "icon-circle-btn interrupt-circle",
                                title: "Stop",
                                onclick: move |_| on_stop.call(()),
                                svg {
                                    view_box: "0 0 24 24",
                                    width: "18",
                                    height: "18",
                                    rect {
                                        x: "6", y: "6", width: "12", height: "12", rx: "2",
                                        fill: "currentColor"
                                    }
                                }
                            }
                        }
                    } else {
                        rsx!( Fragment {} )
                    }}

                    button {
                        class: "icon-circle-btn send-circle",
                        title: "Send (Enter)",
                        disabled: send_disabled,
                        onclick: move |_| do_send(),
                        svg {
                            view_box: "0 0 24 24",
                            width: "20",
                            height: "20",
                            fill: "none",
                            stroke: "currentColor",
                            stroke_width: "2.4",
                            stroke_linecap: "round",
                            stroke_linejoin: "round",
                            path { d: "M12 19V5" }
                            path { d: "M5 12l7-7 7 7" }
                        }
                    }
                }
            }
            div { class: "input-foot",
                p { class: "input-hint",
                    "Enter to send · Shift+Enter for newline · Ctrl+N new chat · Ctrl+B toggle sidebar · Ctrl+, settings"
                }
                {
                    let count = text().chars().count();
                    let near = count >= MAX_MESSAGE_CHARS * 9 / 10;
                    let at_limit = count >= MAX_MESSAGE_CHARS;
                    let mut classes = String::from("char-counter");
                    if near { classes.push_str(" near"); }
                    if at_limit { classes.push_str(" full"); }
                    rsx! {
                        span { class: "{classes}",
                            "{count} / {MAX_MESSAGE_CHARS}"
                        }
                    }
                }
            }
        }
    }
}

/* ================= CHAT WINDOW ================= */

#[component]
fn ChatWindow(
    current_chat_id: Signal<Option<String>>,
    messages: Signal<Vec<(String, String)>>,
    settings: Signal<Settings>,
    chats: Signal<Vec<Chat>>,
) -> Element {
    let mut loading_chat = use_signal(|| Option::<String>::None);
    let mut current_cancel = use_signal(|| Option::<Arc<AtomicBool>>::None);
    let http_client = use_signal(|| {
        Client::builder()
            .build()
            .unwrap_or_else(|_| Client::new())
    });

    let header_title = {
        if let Some(id) = current_chat_id() {
            chats()
                .iter()
                .find(|c| c.id == id)
                .map(|c| c.title.clone())
                .unwrap_or(id.clone())
        } else {
            "No chat selected".to_string()
        }
    };

    let model_display = {
        let s = settings();
        let m = s.model.clone();
        let p = provider_label(&s.provider);
        if m.trim().is_empty() {
            format!("{} · no model", p)
        } else {
            format!("{} · {}", p, m)
        }
    };

    let send_chat = {
        to_owned![
            messages,
            http_client,
            loading_chat,
            current_cancel,
            current_chat_id
        ];
        move |chat_id: String,
              user_message: String,
              settings: Settings,
              cancel_flag: Arc<AtomicBool>| {
            async move {
                if settings.model.trim().is_empty() {
                    let conn = init_db();
                    let db_msg = "Error: No model selected. Open Settings and pick a model first.";
                    conn.execute(
                        "INSERT INTO messages (chat_id, role, content) VALUES (?1, 'assistant', ?2)",
                        params![chat_id, db_msg],
                    )
                    .ok();
                    enforce_history_limit(&conn, &chat_id);

                    if current_chat_id()
                        .as_ref()
                        .map(|c| c == &chat_id)
                        .unwrap_or(false)
                    {
                        messages.push(("assistant".into(), db_msg.to_string()));
                    }

                    loading_chat.set(None);
                    current_cancel.set(None);
                    return;
                }

                // Build initial conversation turns as Vec<Value> so we can append
                // tool-call / tool-result messages when web search is in play.
                let mut turns: Vec<Value> = Vec::new();
                if !settings.system_prompt.trim().is_empty() {
                    turns.push(serde_json::json!({
                        "role": "system",
                        "content": settings.system_prompt.clone(),
                    }));
                }
                for (role, content) in messages().iter() {
                    turns.push(serde_json::json!({
                        "role": role,
                        "content": content,
                    }));
                }
                turns.push(serde_json::json!({
                    "role": "user",
                    "content": user_message.clone(),
                }));

                let viewing = current_chat_id()
                    .as_ref()
                    .map(|c| c == &chat_id)
                    .unwrap_or(false);
                if viewing {
                    messages.push(("assistant".into(), String::new()));
                }

                let stop_vec: Vec<String> = settings
                    .stop_sequences
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();

                let is_ollama = settings.provider == "ollama";
                let tools_enabled = settings.web_search_enabled
                    && is_openai_compatible(&settings.provider)
                    && !settings.tavily_api_key.trim().is_empty();

                let url = format!(
                    "{}{}",
                    settings.api_base.trim_end_matches('/'),
                    provider_chat_path(&settings.provider)
                );

                let build_request = |turns: &Vec<Value>| -> Value {
                    if is_ollama {
                        let mut options = serde_json::json!({
                            "temperature": clamp_f64(settings.temperature, 0.0, 2.0),
                            "top_p": clamp_f64(settings.top_p, 0.0, 1.0),
                            "num_predict": clamp_i32(settings.max_tokens, 1, 1_000_000),
                            "presence_penalty": clamp_f64(settings.presence_penalty, -2.0, 2.0),
                            "frequency_penalty": clamp_f64(settings.frequency_penalty, -2.0, 2.0),
                        });
                        if settings.top_k > 0 {
                            options["top_k"] = serde_json::json!(settings.top_k);
                        }
                        if settings.seed >= 0 {
                            options["seed"] = serde_json::json!(settings.seed);
                        }
                        if !stop_vec.is_empty() {
                            options["stop"] = serde_json::json!(stop_vec);
                        }
                        serde_json::json!({
                            "model": settings.model,
                            "messages": turns,
                            "stream": true,
                            "options": options,
                        })
                    } else {
                        let mut body = serde_json::json!({
                            "model": settings.model,
                            "messages": turns,
                            "stream": true,
                            "temperature": clamp_f64(settings.temperature, 0.0, 2.0),
                            "top_p": clamp_f64(settings.top_p, 0.0, 1.0),
                            "max_tokens": clamp_i32(settings.max_tokens, 1, 1_000_000),
                            "presence_penalty": clamp_f64(settings.presence_penalty, -2.0, 2.0),
                            "frequency_penalty": clamp_f64(settings.frequency_penalty, -2.0, 2.0),
                        });
                        if settings.seed >= 0 {
                            body["seed"] = serde_json::json!(settings.seed);
                        }
                        if !stop_vec.is_empty() {
                            body["stop"] = serde_json::json!(stop_vec);
                        }
                        if tools_enabled {
                            body["tools"] = serde_json::json!([web_search_tool_def()]);
                            body["tool_choice"] = serde_json::json!("auto");
                        }
                        body
                    }
                };

                let mut accum = String::new(); // final visible content
                let mut error_text: Option<String> = None;

                // Tool-calling loop: at most 3 rounds. Without tools_enabled,
                // exits after the first round.
                'rounds: for _round in 0..3u32 {
                    let request_body = build_request(&turns);
                    let mut req = http_client().post(&url).json(&request_body);
                    if is_openai_compatible(&settings.provider) && !settings.api_key.is_empty() {
                        req = req.bearer_auth(&settings.api_key);
                    }

                    let response = match req.send().await {
                        Ok(r) => r,
                        Err(e) => {
                            error_text = Some(format!(
                                "Error: could not reach {} — {}",
                                settings.api_base, e
                            ));
                            break 'rounds;
                        }
                    };
                    if !response.status().is_success() {
                        error_text =
                            Some(format!("Error: API returned status {}", response.status()));
                        break 'rounds;
                    }

                    let mut stream = response.bytes_stream();
                    let mut buf = String::new();
                    let mut round_content = String::new();
                    // (id, name, args_string) per tool_call index
                    let mut tool_calls: Vec<(String, String, String)> = Vec::new();
                    let mut finish_reason: Option<String> = None;

                    'stream: while let Some(chunk) = stream.next().await {
                        if cancel_flag.load(Ordering::Relaxed) {
                            break 'stream;
                        }
                        let bytes = match chunk {
                            Ok(b) => b,
                            Err(e) => {
                                error_text = Some(format!("Error: stream broken — {}", e));
                                break 'stream;
                            }
                        };
                        buf.push_str(&String::from_utf8_lossy(&bytes));

                        if is_ollama {
                            // Ollama path: legacy NDJSON content stream.
                            loop {
                                match extract_ollama_chunk(&mut buf) {
                                    Some((delta, done)) => {
                                        if !delta.is_empty() {
                                            round_content.push_str(&delta);
                                            accum.push_str(&delta);
                                            if current_chat_id()
                                                .as_ref()
                                                .map(|c| c == &chat_id)
                                                .unwrap_or(false)
                                            {
                                                let mut w = messages.write();
                                                if let Some(last) = w.last_mut() {
                                                    if last.0 == "assistant" {
                                                        last.1.push_str(&delta);
                                                    }
                                                }
                                            }
                                        }
                                        if done {
                                            finish_reason = Some("stop".into());
                                            break 'stream;
                                        }
                                    }
                                    None => break,
                                }
                            }
                        } else {
                            // OpenAI-compatible SSE path: read full delta JSON
                            loop {
                                let Some((delta, fin)) = extract_openai_event(&mut buf) else {
                                    break;
                                };
                                if let Some(c) = delta.get("content").and_then(|v| v.as_str()) {
                                    if !c.is_empty() {
                                        round_content.push_str(c);
                                        accum.push_str(c);
                                        if current_chat_id()
                                            .as_ref()
                                            .map(|c2| c2 == &chat_id)
                                            .unwrap_or(false)
                                        {
                                            let mut w = messages.write();
                                            if let Some(last) = w.last_mut() {
                                                if last.0 == "assistant" {
                                                    last.1.push_str(c);
                                                }
                                            }
                                        }
                                    }
                                }
                                if let Some(tc_arr) =
                                    delta.get("tool_calls").and_then(|v| v.as_array())
                                {
                                    // Show searching indicator
                                    if current_chat_id()
                                        .as_ref()
                                        .map(|c| c == &chat_id)
                                        .unwrap_or(false)
                                        && round_content.is_empty()
                                    {
                                        let mut w = messages.write();
                                        if let Some(last) = w.last_mut() {
                                            if last.0 == "assistant" && last.1.is_empty() {
                                                last.1 = "Searching the web…".to_string();
                                            }
                                        }
                                    }
                                    for tc in tc_arr {
                                        let idx = tc
                                            .get("index")
                                            .and_then(|v| v.as_u64())
                                            .unwrap_or(0)
                                            as usize;
                                        while tool_calls.len() <= idx {
                                            tool_calls.push((
                                                String::new(),
                                                String::new(),
                                                String::new(),
                                            ));
                                        }
                                        if let Some(id) =
                                            tc.get("id").and_then(|v| v.as_str())
                                        {
                                            if !id.is_empty() {
                                                tool_calls[idx].0 = id.to_string();
                                            }
                                        }
                                        if let Some(func) = tc.get("function") {
                                            if let Some(name) =
                                                func.get("name").and_then(|v| v.as_str())
                                            {
                                                if !name.is_empty() {
                                                    tool_calls[idx].1 = name.to_string();
                                                }
                                            }
                                            if let Some(args) =
                                                func.get("arguments").and_then(|v| v.as_str())
                                            {
                                                tool_calls[idx].2.push_str(args);
                                            }
                                        }
                                    }
                                }
                                if let Some(reason) = fin {
                                    finish_reason = Some(reason.clone());
                                    if reason == "[DONE]"
                                        || reason == "stop"
                                        || reason == "length"
                                        || reason == "tool_calls"
                                    {
                                        break 'stream;
                                    }
                                }
                            }
                        }
                    }

                    if cancel_flag.load(Ordering::Relaxed) {
                        break 'rounds;
                    }

                    // Decide whether to loop again (tool round) or finish
                    if finish_reason.as_deref() == Some("tool_calls")
                        && !tool_calls.is_empty()
                        && tools_enabled
                    {
                        // Persist assistant tool_calls turn (in-memory only)
                        let tc_json: Vec<Value> = tool_calls
                            .iter()
                            .map(|(id, name, args)| {
                                serde_json::json!({
                                    "id": id,
                                    "type": "function",
                                    "function": { "name": name, "arguments": args }
                                })
                            })
                            .collect();
                        turns.push(serde_json::json!({
                            "role": "assistant",
                            "content": Value::Null,
                            "tool_calls": tc_json,
                        }));
                        // Execute each tool, append role:"tool" results
                        for (id, name, args) in &tool_calls {
                            let result =
                                execute_tool_call(&http_client(), &settings, name, args).await;
                            turns.push(serde_json::json!({
                                "role": "tool",
                                "tool_call_id": id,
                                "content": result,
                            }));
                        }
                        // Reset visible placeholder so next round's content streams cleanly
                        if current_chat_id()
                            .as_ref()
                            .map(|c| c == &chat_id)
                            .unwrap_or(false)
                        {
                            let mut w = messages.write();
                            if let Some(last) = w.last_mut() {
                                if last.0 == "assistant" && last.1 == "Searching the web…" {
                                    last.1.clear();
                                }
                            }
                        }
                        // Continue loop
                    } else {
                        // Final round complete
                        break 'rounds;
                    }
                }

                let conn = init_db();
                if let Some(err) = error_text {
                    // Replace any partial placeholder in UI with the error
                    if current_chat_id()
                        .as_ref()
                        .map(|c| c == &chat_id)
                        .unwrap_or(false)
                    {
                        let mut w = messages.write();
                        if let Some(last) = w.last_mut() {
                            if last.0 == "assistant" && last.1.is_empty() {
                                last.1 = err.clone();
                            } else {
                                drop(w);
                                messages.push(("assistant".into(), err.clone()));
                            }
                        } else {
                            drop(w);
                            messages.push(("assistant".into(), err.clone()));
                        }
                    }
                    let _ = conn.execute(
                        "INSERT INTO messages (chat_id, role, content) VALUES (?1, 'assistant', ?2)",
                        params![chat_id, err],
                    );
                } else if !accum.is_empty() {
                    let _ = conn.execute(
                        "INSERT INTO messages (chat_id, role, content) VALUES (?1, 'assistant', ?2)",
                        params![chat_id, accum],
                    );
                } else {
                    // No content (cancelled before any token, or empty stream): drop placeholder
                    if current_chat_id()
                        .as_ref()
                        .map(|c| c == &chat_id)
                        .unwrap_or(false)
                    {
                        let mut w = messages.write();
                        if let Some(last) = w.last() {
                            if last.0 == "assistant" && last.1.is_empty() {
                                w.pop();
                            }
                        }
                    }
                }
                enforce_history_limit(&conn, &chat_id);

                loading_chat.set(None);
                current_cancel.set(None);
            }
        }
    };

    let is_loading_current = loading_chat()
        .as_ref()
        .map(|l| current_chat_id().as_ref().map(|c| c == l).unwrap_or(false))
        .unwrap_or(false);

    rsx! {
        div { class: "chat-window",

            div { class: "chat-header",
                button {
                    class: "header-icon-btn",
                    title: "Toggle sidebar (Ctrl+B)",
                    onclick: move |_| {
                        let mut s = settings();
                        s.sidebar_collapsed = !s.sidebar_collapsed;
                        let conn = init_db();
                        save_settings(&conn, &s);
                        settings.set(s);
                    },
                    svg {
                        view_box: "0 0 24 24", width: "20", height: "20",
                        fill: "none", stroke: "currentColor", stroke_width: "2",
                        stroke_linecap: "round", stroke_linejoin: "round",
                        rect { x: "3", y: "4", width: "18", height: "16", rx: "2" }
                        path { d: "M9 4v16" }
                    }
                }
                div { class: "chat-header-text",
                    h2 { "{header_title}" }
                    p { class: "model-indicator", "{model_display}" }
                }
                button {
                    class: "theme-toggle",
                    title: if settings().theme == "dark" { "Switch to light" } else { "Switch to dark" },
                    onclick: move |_| {
                        let mut s = settings();
                        s.theme = if s.theme == "dark" { "light".to_string() } else { "dark".to_string() };
                        let conn = init_db();
                        save_settings(&conn, &s);
                        settings.set(s);
                    },
                    {
                        if settings().theme == "dark" {
                            // sun icon — switch to light
                            rsx! {
                                svg {
                                    view_box: "0 0 24 24", width: "22", height: "22",
                                    fill: "none", stroke: "currentColor", stroke_width: "2",
                                    stroke_linecap: "round", stroke_linejoin: "round",
                                    circle { cx: "12", cy: "12", r: "4" }
                                    path { d: "M12 2v2" }
                                    path { d: "M12 20v2" }
                                    path { d: "M4.93 4.93l1.41 1.41" }
                                    path { d: "M17.66 17.66l1.41 1.41" }
                                    path { d: "M2 12h2" }
                                    path { d: "M20 12h2" }
                                    path { d: "M6.34 17.66l-1.41 1.41" }
                                    path { d: "M19.07 4.93l-1.41 1.41" }
                                }
                            }
                        } else {
                            // moon icon — switch to dark
                            rsx! {
                                svg {
                                    view_box: "0 0 24 24", width: "22", height: "22",
                                    fill: "none", stroke: "currentColor", stroke_width: "2",
                                    stroke_linecap: "round", stroke_linejoin: "round",
                                    path { d: "M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z" }
                                }
                            }
                        }
                    }
                }
            }

            div { class: "chat-messages",
                {
                    let user = use_context::<Signal<User>>()();
                    let user_label_str = if user.is_guest { "Guest".to_string() } else { user.username.clone() };
                    let user_avatar_str = user.avatar_data.clone();
                    let provider_text = provider_label(&settings().provider).to_string();
                    messages().iter().enumerate().map(move |(i, (role, content))| {
                        rsx! {
                            Message {
                                key: "{i}",
                                role: role.clone(),
                                content: content.clone(),
                                user_avatar: user_avatar_str.clone(),
                                user_label: user_label_str.clone(),
                                provider_label: provider_text.clone(),
                            }
                        }
                    })
                }

                { if is_loading_current && messages().last().map(|m| m.1.is_empty()).unwrap_or(true) {
                    rsx! {
                        div { class: "message assistant-message loading-message",
                            span { class: "loading-dots",
                                span {}
                                span {}
                                span {}
                            }
                        }
                    }
                } else {
                    rsx!( Fragment {} )
                }}
            }

            MessageComposer {
                is_loading: is_loading_current,
                disabled_external: current_chat_id().is_none()
                    || loading_chat()
                        .as_ref()
                        .map(|l| current_chat_id().as_ref().map(|c| c != l).unwrap_or(false))
                        .unwrap_or(false),
                settings: settings.clone(),
                on_send: {
                    let mut messages = messages.clone();
                    let mut current_cancel = current_cancel.clone();
                    let mut loading_chat = loading_chat.clone();
                    let send_chat = send_chat.clone();
                    let settings = settings.clone();
                    let current_chat_id = current_chat_id.clone();
                    move |text: String| {
                        let Some(chat_id) = current_chat_id() else { return };
                        if text.trim().is_empty() { return; }
                        let conn = init_db();
                        let user_text = sanitize_multiline(&text, MAX_MESSAGE_CHARS);
                        conn.execute(
                            "INSERT INTO messages (chat_id, role, content)
                             VALUES (?1, 'user', ?2)",
                            params![chat_id, user_text.clone()],
                        ).unwrap();
                        enforce_history_limit(&conn, &chat_id);
                        messages.push(("user".into(), user_text.clone()));
                        let cancel_flag = Arc::new(AtomicBool::new(false));
                        current_cancel.set(Some(cancel_flag.clone()));
                        loading_chat.set(Some(chat_id.clone()));
                        spawn({
                            let chat_id = chat_id.clone();
                            let settings_snapshot = settings();
                            let cancel_flag = cancel_flag.clone();
                            send_chat(chat_id, user_text, settings_snapshot, cancel_flag)
                        });
                    }
                },
                on_stop: move |_| {
                    if let Some(cancel) = current_cancel() {
                        cancel.store(true, Ordering::Relaxed);
                    }
                },
            }
        }
    }
}

/* ================= MESSAGE ================= */

#[component]
fn Message(
    role: String,
    content: String,
    user_avatar: Option<String>,
    user_label: String,
    provider_label: String,
) -> Element {
    let is_user = role == "user";
    let class_name = if is_user {
        "message user-message"
    } else {
        "message assistant-message"
    };
    let row_class = if is_user { "message-row message-row-user" } else { "message-row message-row-assistant" };

    let (avatar_src, avatar_label) = if is_user {
        (user_avatar.clone(), user_label.clone())
    } else {
        (None, provider_label.clone())
    };
    let bg = color_for(&avatar_label);
    let initial = initial_for(&avatar_label);

    let avatar = rsx! {
        {
            if let Some(src) = avatar_src.clone() {
                rsx! { img { class: "avatar avatar-msg", src: "{src}", alt: "" } }
            } else {
                rsx! {
                    span { class: "avatar avatar-msg avatar-initial",
                        style: "background: {bg}",
                        title: "{avatar_label}",
                        "{initial}"
                    }
                }
            }
        }
    };

    let bubble = if content.contains("<think>") && content.contains("</think>") {
        let think_start = content.find("<think>").unwrap() + "<think>".len();
        let think_end = content.find("</think>").unwrap();
        let think_content = content[think_start..think_end].trim().to_string();
        let before_think = content[..think_start - "<think>".len()].to_string();
        let after_think = content[think_end + "</think>".len()..].to_string();
        rsx! {
            div { class: "{class_name}",
                {if !before_think.is_empty() {
                    rsx! { p { class: "msg-text", "{before_think}" } }
                } else {
                    rsx! { Fragment {} }
                }}
                div { class: "think-bubble",
                    p { class: "think-label", "Reasoning" }
                    div { class: "think-content", "{think_content}" }
                }
                {if !after_think.is_empty() {
                    rsx! { p { class: "msg-text", "{after_think}" } }
                } else {
                    rsx! { Fragment {} }
                }}
            }
        }
    } else {
        rsx! {
            div { class: "{class_name}",
                p { class: "msg-text", "{content}" }
            }
        }
    };

    rsx! {
        div { class: "{row_class}",
            {avatar}
            {bubble}
        }
    }
}
