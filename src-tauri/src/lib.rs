use std::io::{Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};

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
        Some("gif") => "image/gif",
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

// ── File reading ──────────────────────────────────────────────────────────────

fn read_bytes(path: &std::path::Path, start: u64, end: u64) -> std::io::Result<Vec<u8>> {
    let mut file = std::fs::File::open(path)?;
    file.seek(SeekFrom::Start(start))?;
    let length = (end - start + 1) as usize;
    let mut buf = vec![0u8; length];
    let mut offset = 0;
    while offset < length {
        match file.read(&mut buf[offset..]) {
            Ok(0) => break,
            Ok(n) => offset += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    buf.truncate(offset);
    Ok(buf)
}

// ── HTTP video server ─────────────────────────────────────────────────────────

fn send_raw(stream: &mut TcpStream, head: String, body: &[u8]) {
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
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

    match read_bytes(path, start, end) {
        Ok(data) => {
            let is_range = range_header.is_some();
            let status = if is_range { "206 Partial Content" } else { "200 OK" };
            let mut head = format!(
                "HTTP/1.1 {status}\r\n\
                Content-Type: {mime}\r\n\
                Content-Length: {}\r\n\
                Accept-Ranges: bytes\r\n\
                Access-Control-Allow-Origin: *\r\n\
                Access-Control-Expose-Headers: Content-Length, Content-Range\r\n",
                data.len()
            );
            if is_range {
                head.push_str(&format!("Content-Range: bytes {start}-{end}/{file_size}\r\n"));
            }
            head.push_str("\r\n");
            send_raw(&mut stream, head, &data);
        }
        Err(_) => {
            let _ = stream.write_all(
                b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n",
            );
        }
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

// ── Commands ──────────────────────────────────────────────────────────────────

#[tauri::command]
fn get_server_port(state: tauri::State<VideoServerPort>) -> u16 {
    state.0
}

#[tauri::command]
async fn pick_video(app: tauri::AppHandle) -> Result<String, String> {
    use tauri_plugin_dialog::DialogExt;
    use std::sync::mpsc;

    let (tx, rx) = mpsc::channel();

    app.dialog()
        .file()
        .add_filter(
            "Video / GIF",
            &["mp4", "m4v", "mkv", "avi", "mov", "webm", "flv", "wmv", "gif"],
        )
        .pick_file(move |result| {
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

    tauri::Builder::default()
        .manage(VideoServerPort(port))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .invoke_handler(tauri::generate_handler![pick_video, get_server_port])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
