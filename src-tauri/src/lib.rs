use std::io::{Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Mutex;

// ── MIME ─────────────────────────────────────────────────────────────────────

fn mime_for_path(path: &std::path::Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_lowercase())
        .as_deref()
    {
        Some("mp4") | Some("m4v") => "video/mp4",
        Some("webm") => "video/webm",
        Some("mkv") => "video/x-matroska",
        Some("avi") => "video/x-msvideo",
        Some("mov") | Some("qt") => "video/quicktime",
        Some("flv") => "video/x-flv",
        Some("wmv") => "video/x-ms-wmv",
        _ => "application/octet-stream",
    }
}

// ── Range parsing ─────────────────────────────────────────────────────────────

fn parse_range(range_str: &str, file_size: u64) -> Option<(u64, u64)> {
    let s = range_str.strip_prefix("bytes=")?;
    let dash = s.find('-')?;
    let start_str = &s[..dash];
    let end_str = &s[dash + 1..];

    let (start, end) = if start_str.is_empty() {
        let suffix: u64 = end_str.parse().ok()?;
        (file_size.saturating_sub(suffix), file_size - 1)
    } else {
        let start: u64 = start_str.parse().ok()?;
        let end = if end_str.is_empty() {
            file_size - 1
        } else {
            end_str.parse::<u64>().ok()?.min(file_size - 1)
        };
        (start, end)
    };

    if start > end || start >= file_size {
        return None;
    }
    Some((start, end))
}

// ── HTTP video server ─────────────────────────────────────────────────────────

const CHUNK: usize = 512 * 1024; // 512 KB

fn stream_body(stream: &mut TcpStream, path: &std::path::Path, start: u64, end: u64) {
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return,
    };
    if file.seek(SeekFrom::Start(start)).is_err() {
        return;
    }
    let mut remaining = end - start + 1;
    let mut buf = vec![0u8; CHUNK];
    while remaining > 0 {
        let to_read = (remaining as usize).min(CHUNK);
        match file.read(&mut buf[..to_read]) {
            Ok(0) => break,
            Ok(n) => {
                if stream.write_all(&buf[..n]).is_err() {
                    break;
                }
                remaining -= n as u64;
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    let _ = stream.flush();
}

fn handle_http(mut stream: TcpStream) {
    // Only accept loopback connections
    if stream.peer_addr().map(|a| !a.ip().is_loopback()).unwrap_or(true) {
        return;
    }

    let mut buf = vec![0u8; 8192];
    let n = match stream.read(&mut buf) {
        Ok(n) if n > 0 => n,
        _ => return,
    };
    let req = std::str::from_utf8(&buf[..n]).unwrap_or("");

    // OPTIONS preflight
    let first_line = req.lines().next().unwrap_or("");
    if first_line.starts_with("OPTIONS") {
        let head = "HTTP/1.1 204 No Content\r\n\
            Access-Control-Allow-Origin: *\r\n\
            Access-Control-Allow-Methods: GET, OPTIONS\r\n\
            Access-Control-Allow-Headers: *\r\n\
            Content-Length: 0\r\n\r\n"
            .to_string();
        let _ = stream.write_all(head.as_bytes());
        return;
    }

    // Parse URL from first line: GET /video?path=... HTTP/1.1
    let url = first_line.split_whitespace().nth(1).unwrap_or("/");

    let encoded_path = url
        .split('?')
        .nth(1)
        .and_then(|q| q.split('&').find(|p| p.starts_with("path=")))
        .and_then(|p| p.strip_prefix("path="));

    let path_str = match encoded_path.and_then(|e| urlencoding::decode(e).ok()) {
        Some(p) => p.into_owned(),
        None => {
            let _ = stream.write_all(
                b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n",
            );
            return;
        }
    };

    // Strip file:// prefix if dialog returned a URL instead of a path
    let path_str = path_str
        .trim()
        .strip_prefix("file://")
        .unwrap_or(&path_str)
        .to_string();

    let path = std::path::Path::new(&path_str);

    // Parse Range header
    let range_header = req
        .lines()
        .skip(1)
        .take_while(|l| !l.trim().is_empty())
        .find(|l| l.to_lowercase().starts_with("range:"))
        .map(|l| l[6..].trim().to_string());

    let file_size = match std::fs::metadata(path) {
        Ok(m) => m.len(),
        Err(_) => {
            let _ = stream.write_all(
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n",
            );
            return;
        }
    };

    if file_size == 0 {
        let _ = stream.write_all(
            b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n",
        );
        return;
    }

    let mime = mime_for_path(path);

    let (start, end) = if let Some(ref range) = range_header {
        match parse_range(range, file_size) {
            Some(r) => r,
            None => {
                let head = format!(
                    "HTTP/1.1 416 Range Not Satisfiable\r\n\
                    Content-Range: bytes */{file_size}\r\n\
                    Access-Control-Allow-Origin: *\r\n\
                    Content-Length: 0\r\n\r\n"
                );
                let _ = stream.write_all(head.as_bytes());
                return;
            }
        }
    } else {
        (0, file_size - 1)
    };

    let is_range = range_header.is_some();
    let content_length = end - start + 1;
    let status = if is_range { "206 Partial Content" } else { "200 OK" };
    let mut head = format!(
        "HTTP/1.1 {status}\r\n\
        Content-Type: {mime}\r\n\
        Content-Length: {content_length}\r\n\
        Accept-Ranges: bytes\r\n\
        Access-Control-Allow-Origin: *\r\n\
        Access-Control-Expose-Headers: Content-Length, Content-Range\r\n"
    );
    if is_range {
        head.push_str(&format!("Content-Range: bytes {start}-{end}/{file_size}\r\n"));
    }
    head.push_str("\r\n");
    if stream.write_all(head.as_bytes()).is_ok() {
        stream_body(&mut stream, path, start, end);
    }
}

fn start_video_server() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("Failed to bind video server");
    let port = listener.local_addr().unwrap().port();

    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || handle_http(stream));
        }
    });

    port
}

// ── Tauri managed state ───────────────────────────────────────────────────────

struct VideoServerPort(u16);
struct InitialFile(Mutex<Option<String>>);

// ── Commands ──────────────────────────────────────────────────────────────────

#[tauri::command]
fn get_server_port(state: tauri::State<VideoServerPort>) -> u16 {
    state.0
}

#[tauri::command]
fn get_initial_file(state: tauri::State<InitialFile>) -> Option<String> {
    state.0.lock().unwrap().take()
}

#[tauri::command]
async fn pick_video(app: tauri::AppHandle, start_dir: Option<String>) -> Result<String, String> {
    use tauri_plugin_dialog::DialogExt;
    use std::sync::mpsc;

    let (tx, rx) = mpsc::channel();

    let mut dialog = app
        .dialog()
        .file()
        .add_filter("Video", &["mp4", "m4v", "mkv", "avi", "mov", "webm", "flv", "wmv"]);

    if let Some(dir) = start_dir {
        let p = std::path::PathBuf::from(&dir);
        if p.is_dir() {
            dialog = dialog.set_directory(p);
        }
    }

    dialog.pick_file(move |result| {
        let _ = tx.send(result);
    });

    tauri::async_runtime::spawn_blocking(move || rx.recv().ok().flatten())
        .await
        .map_err(|e| e.to_string())?
        .map(|fp| fp.to_string())
        .ok_or_else(|| "cancelled".to_string())
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let port = start_video_server();

    // Linux: la app recibe el archivo como argumento CLI al hacer doble clic
    #[cfg(target_os = "linux")]
    let initial_file = std::env::args()
        .skip(1)
        .find(|a| !a.starts_with('-') && std::path::Path::new(a).exists());

    #[cfg(not(target_os = "linux"))]
    let initial_file: Option<String> = None;

    tauri::Builder::default()
        .manage(VideoServerPort(port))
        .manage(InitialFile(Mutex::new(initial_file)))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .invoke_handler(tauri::generate_handler![pick_video, get_server_port, get_initial_file])
        .build(tauri::generate_context!())
        .expect("error while running tauri application")
        .run(|_app, _event| {
            // macOS: el OS envía RunEvent::Opened en vez de args CLI
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Opened { urls } = _event {
                use tauri::Emitter;
                let paths: Vec<String> = urls
                    .iter()
                    .filter_map(|u| u.to_file_path().ok())
                    .filter_map(|p| p.to_str().map(String::from))
                    .collect();

                // Cold start: guardar para que el frontend lo lea al montar
                if let Some(first) = paths.first() {
                    *_app.state::<InitialFile>().0.lock().unwrap() = Some(first.clone());
                }
                // Warm start: app ya abierta, notificar directamente
                let _ = _app.emit("opened", paths);
            }
        });
}
