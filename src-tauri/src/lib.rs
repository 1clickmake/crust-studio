use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use tauri::{Manager, RunEvent, State};
use tauri_plugin_opener::OpenerExt;
use walkdir::WalkDir;
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

#[derive(Default)]
struct StudioRuntime {
    workspace_path: Option<PathBuf>,
    child: Option<Child>,
    port: u16,
    logs: VecDeque<String>,
}

struct AppState(Mutex<StudioRuntime>);

#[derive(Serialize)]
struct ThemeInfo {
    slug: String,
    name: String,
    author: String,
    version: String,
    is_builtin: bool,
}

#[derive(Serialize)]
struct StudioStatus {
    workspace_path: String,
    workspace_valid: bool,
    has_binary: bool,
    running: bool,
    port: u16,
    url: String,
    dev_mode: bool,
    themes: Vec<ThemeInfo>,
    logs: Vec<String>,
}

#[derive(Serialize, Deserialize, Default)]
struct SavedPrefs {
    workspace_path: Option<String>,
}

fn prefs_file_path() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("crust-studio.json")))
}

fn detect_default_workspace() -> Option<PathBuf> {
    if let Some(pf) = prefs_file_path() {
        if let Ok(raw) = fs::read_to_string(&pf) {
            if let Ok(prefs) = serde_json::from_str::<SavedPrefs>(&raw) {
                if let Some(p) = prefs.workspace_path {
                    let pb = PathBuf::from(p);
                    if pb.join("templates").exists() {
                        return Some(pb);
                    }
                }
            }
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let mut candidates = vec![dir.join("runtime"), dir.join("crust"), dir.to_path_buf()];
            // Inside macOS .app bundle: MyApp.app/Contents/MacOS/exe -> check folder containing .app
            if let Some(bundle_parent) = dir.parent().and_then(|p| p.parent()).and_then(|p| p.parent()) {
                candidates.push(bundle_parent.join("runtime"));
                candidates.push(bundle_parent.join("crust"));
            }
            for cand in candidates {
                if cand.join("templates").exists() {
                    return Some(cand);
                }
            }
        }
    }

    if let Ok(cwd) = std::env::current_dir() {
        for cand in [
            cwd.join("runtime"),
            cwd.join("..").join("rust_board"),
            cwd.join("..").join("..").join("rust_board"),
        ] {
            if cand.join("templates").exists() {
                return cand.canonicalize().ok().or(Some(cand));
            }
        }
    }

    None
}

fn find_crust_binary(ws: &Path) -> Option<PathBuf> {
    let exe_name = if cfg!(windows) { "rust_board.exe" } else { "rust_board" };
    let alt_name = if cfg!(windows) { "crust.exe" } else { "crust" };

    let candidates = [
        ws.join(alt_name),
        ws.join(exe_name),
        ws.join("target").join("release").join(exe_name),
        ws.join("target").join("debug").join(exe_name),
    ];

    candidates.into_iter().find(|p| p.is_file())
}

fn read_config_port_and_dev_mode(ws: &Path) -> (u16, bool) {
    let cfg_path = ws.join("config.toml");
    let raw = fs::read_to_string(cfg_path).unwrap_or_default();
    let mut port = 3000u16;
    let mut dev_mode = false;
    for line in raw.lines() {
        let t = line.trim();
        if t.starts_with('#') {
            continue;
        }
        if let Some(rest) = t.strip_prefix("port") {
            if let Some((_, val)) = rest.split_once('=') {
                if let Ok(p) = val.trim().parse::<u16>() {
                    port = p;
                }
            }
        } else if let Some(rest) = t.strip_prefix("dev_mode") {
            if let Some((_, val)) = rest.split_once('=') {
                dev_mode = val.trim() == "true";
            }
        }
    }
    (port, dev_mode)
}

fn parse_theme_toml(path: &Path, slug: &str) -> ThemeInfo {
    let raw = fs::read_to_string(path.join("theme.toml")).unwrap_or_default();
    let mut name = slug.to_string();
    let mut author = "Unknown".to_string();
    let mut version = "1.0.0".to_string();

    for line in raw.lines() {
        let t = line.trim();
        if let Some((k, v)) = t.split_once('=') {
            let key = k.trim();
            let val = v.trim().trim_matches('"').trim_matches('\'').to_string();
            match key {
                "name" => name = val,
                "author" => author = val,
                "version" => version = val,
                _ => {}
            }
        }
    }

    ThemeInfo {
        slug: slug.to_string(),
        name,
        author,
        version,
        is_builtin: slug == "default" || slug == "success",
    }
}

fn list_themes(ws: &Path) -> Vec<ThemeInfo> {
    let dir = ws.join("templates").join("themes");
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                let slug = entry.file_name().to_string_lossy().to_string();
                if !slug.starts_with('.') {
                    out.push(parse_theme_toml(&entry.path(), &slug));
                }
            }
        }
    }
    out.sort_by(|a, b| a.slug.cmp(&b.slug));
    out
}

fn push_log(logs: &mut VecDeque<String>, line: String) {
    if logs.len() >= 300 {
        logs.pop_front();
    }
    logs.push_back(line);
}

fn build_status(rt: &mut StudioRuntime) -> StudioStatus {
    if let Some(child) = rt.child.as_mut() {
        if let Ok(Some(code)) = child.try_wait() {
            push_log(&mut rt.logs, format!("[Studio] CRust server exited ({code})"));
            rt.child = None;
        }
    }

    let ws_opt = rt.workspace_path.clone();
    let (workspace_path, workspace_valid, has_binary, port, dev_mode, themes) =
        if let Some(ws) = ws_opt {
            let valid = ws.join("templates").exists();
            let has_bin = find_crust_binary(&ws).is_some();
            let (cfg_port, dev) = read_config_port_and_dev_mode(&ws);
            let p = if rt.child.is_some() && rt.port > 0 {
                rt.port
            } else {
                cfg_port
            };
            let t = if valid { list_themes(&ws) } else { Vec::new() };
            (ws.to_string_lossy().to_string(), valid, has_bin, p, dev, t)
        } else {
            (String::new(), false, false, 3000, false, Vec::new())
        };

    let running = rt.child.is_some();
    StudioStatus {
        workspace_path,
        workspace_valid,
        has_binary,
        running,
        port,
        url: format!("http://127.0.0.1:{}", port),
        dev_mode,
        themes,
        logs: rt.logs.iter().cloned().collect(),
    }
}

#[tauri::command]
fn get_studio_state(state: State<AppState>) -> Result<StudioStatus, String> {
    let mut rt = state.0.lock().map_err(|e| e.to_string())?;
    if rt.workspace_path.is_none() {
        rt.workspace_path = detect_default_workspace();
    }
    Ok(build_status(&mut rt))
}

#[tauri::command]
fn set_workspace_path(state: State<AppState>, path: String) -> Result<StudioStatus, String> {
    let pb = PathBuf::from(path.trim());
    if !pb.join("templates").exists() {
        return Err("선택한 폴더에 templates 디렉토리가 없습니다. CRust 루트 폴더를 지정해 주세요.".into());
    }
    if let Some(pf) = prefs_file_path() {
        let prefs = SavedPrefs {
            workspace_path: Some(pb.to_string_lossy().to_string()),
        };
        let _ = fs::write(pf, serde_json::to_string_pretty(&prefs).unwrap_or_default());
    }
    let mut rt = state.0.lock().map_err(|e| e.to_string())?;
    rt.workspace_path = Some(pb);
    push_log(&mut rt.logs, "[Studio] 작업 폴더가 업데이트되었습니다.".into());
    Ok(build_status(&mut rt))
}

#[tauri::command]
fn install_crust(app: tauri::AppHandle, state: State<AppState>, target_dir: String) -> Result<StudioStatus, String> {
    let dest = PathBuf::from(target_dir.trim());
    if dest.as_os_str().is_empty() {
        return Err("설치할 폴더 경로를 입력해 주세요.".into());
    }

    {
        let mut rt = state.0.lock().map_err(|e| e.to_string())?;
        push_log(
            &mut rt.logs,
            format!("[Studio] CRust 다운로드 및 설치를 시작합니다 -> {}", dest.display()),
        );
    }

    let client = reqwest::blocking::Client::builder()
        .user_agent("CRust-Studio/0.1")
        .build()
        .map_err(|e| format!("HTTP 클라이언트 오류: {e}"))?;

    // 1) Try latest GitHub Release asset matching current OS/Arch, fallback to main.zip source
    let os_tag = if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            "macos-arm64"
        } else {
            "macos-x64"
        }
    } else {
        "linux"
    };

    let mut download_url = String::from("https://github.com/1clickmake/CRust/archive/refs/heads/main.zip");
    let mut is_release_asset = false;

    if let Ok(resp) = client
        .get("https://api.github.com/repos/1clickmake/CRust/releases/latest")
        .send()
    {
        if resp.status().is_success() {
            if let Ok(json) = resp.json::<serde_json::Value>() {
                if let Some(assets) = json.get("assets").and_then(|v| v.as_array()) {
                    for asset in assets {
                        let name = asset.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        if name.to_lowercase().contains(os_tag) && name.ends_with(".zip") {
                            if let Some(u) = asset.get("browser_download_url").and_then(|v| v.as_str()) {
                                download_url = u.to_string();
                                is_release_asset = true;
                                break;
                            }
                        }
                    }
                }
            }
        }
    }

    {
        let mut rt = state.0.lock().map_err(|e| e.to_string())?;
        push_log(&mut rt.logs, format!("[Studio] 다운로드: {}", download_url));
    }

    let bytes = client
        .get(&download_url)
        .send()
        .map_err(|e| format!("다운로드 요청 실패: {e}"))?
        .error_for_status()
        .map_err(|e| format!("다운로드 응답 오류: {e}"))?
        .bytes()
        .map_err(|e| format!("다운로드 데이터 읽기 실패: {e}"))?;

    fs::create_dir_all(&dest).map_err(|e| format!("폴더 생성 실패: {e}"))?;
    let reader = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(reader).map_err(|e| format!("ZIP 압축 해제 오류: {e}"))?;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i).map_err(|e| e.to_string())?;
        let raw_name = file.name().replace('\\', "/");
        // GitHub archive wraps everything in `CRust-main/`; strip top folder for source archive
        let rel_str = if !is_release_asset {
            match raw_name.split_once('/') {
                Some((_, rest)) if !rest.is_empty() => rest.to_string(),
                _ => continue,
            }
        } else {
            raw_name
        };

        let rel_path = Path::new(&rel_str);
        if rel_path.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
            continue;
        }
        let out_path = dest.join(rel_path);
        if file.is_dir() {
            fs::create_dir_all(&out_path).map_err(|e| e.to_string())?;
        } else {
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let mut outfile = File::create(&out_path).map_err(|e| e.to_string())?;
            std::io::copy(&mut file, &mut outfile).map_err(|e| e.to_string())?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Some(fname) = out_path.file_name().and_then(|n| n.to_str()) {
                    if fname == "rust_board" || fname == "crust" {
                        if let Ok(mut perms) = fs::metadata(&out_path).map(|m| m.permissions()) {
                            perms.set_mode(0o755);
                            let _ = fs::set_permissions(&out_path, perms);
                        }
                    }
                }
            }
        }
    }

    // Prepare config.toml with dev_mode = true if missing
    let cfg_path = dest.join("config.toml");
    if !cfg_path.exists() {
        let example = dest.join("config.example.toml");
        if example.exists() {
            let raw = fs::read_to_string(&example).unwrap_or_default();
            let patched = raw.replace("dev_mode = false", "dev_mode = true");
            let _ = fs::write(&cfg_path, patched);
        }
    }

    if let Some(pf) = prefs_file_path() {
        let prefs = SavedPrefs {
            workspace_path: Some(dest.to_string_lossy().to_string()),
        };
        let _ = fs::write(pf, serde_json::to_string_pretty(&prefs).unwrap_or_default());
    }

    let mut rt = state.0.lock().map_err(|e| e.to_string())?;
    rt.workspace_path = Some(dest.clone());
    push_log(
        &mut rt.logs,
        format!("[Studio] CRust 설치 완료! 이제 '서버 시작' 버튼을 누르시면 됩니다."),
    );
    let _ = app;
    Ok(build_status(&mut rt))
}

#[tauri::command]
fn toggle_dev_mode(state: State<AppState>, enabled: bool) -> Result<StudioStatus, String> {
    let mut rt = state.0.lock().map_err(|e| e.to_string())?;
    let ws = rt
        .workspace_path
        .clone()
        .ok_or_else(|| "먼저 CRust 작업 폴더를 지정해 주세요.".to_string())?;
    let cfg_path = ws.join("config.toml");
    let raw = fs::read_to_string(&cfg_path).map_err(|e| format!("config.toml 읽기 실패: {e}"))?;

    let target_val = if enabled { "true" } else { "false" };
    let mut replaced = false;
    let mut new_lines = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('#') && trimmed.starts_with("dev_mode") && trimmed.contains('=') {
            new_lines.push(format!("dev_mode = {}", target_val));
            replaced = true;
        } else {
            new_lines.push(line.to_string());
        }
    }
    if !replaced {
        new_lines.push(format!("dev_mode = {}", target_val));
    }
    fs::write(&cfg_path, new_lines.join("\n") + "\n")
        .map_err(|e| format!("config.toml 저장 실패: {e}"))?;

    push_log(
        &mut rt.logs,
        format!("[Studio] dev_mode = {} 로 변경됨 (서버 재시작 시 적용)", target_val),
    );
    Ok(build_status(&mut rt))
}

fn find_cargo_binary() -> Option<PathBuf> {
    if let Ok(home) = std::env::var("HOME") {
        let p = PathBuf::from(home).join(".cargo").join("bin").join("cargo");
        if p.is_file() {
            return Some(p);
        }
    }
    for cand in ["/opt/homebrew/bin/cargo", "/usr/local/bin/cargo"] {
        let p = PathBuf::from(cand);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

#[tauri::command]
fn start_server(
    app: tauri::AppHandle,
    state: State<AppState>,
    open_browser: bool,
) -> Result<StudioStatus, String> {
    let mut rt = state.0.lock().map_err(|e| e.to_string())?;
    if rt.child.is_some() {
        return Ok(build_status(&mut rt));
    }

    let ws = rt
        .workspace_path
        .clone()
        .ok_or_else(|| "먼저 CRust 작업 폴더를 지정해 주세요.".to_string())?;

    let (port, _) = read_config_port_and_dev_mode(&ws);
    if TcpListener::bind(("127.0.0.1", port)).is_err() {
        return Err(format!(
            "포트 {}가 이미 사용 중입니다. 기존 서버를 종료하거나 config.toml 포트를 변경해 주세요.",
            port
        ));
    }

    let mut cmd = if let Some(bin) = find_crust_binary(&ws) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(mut perms) = fs::metadata(&bin).map(|m| m.permissions()) {
                if (perms.mode() & 0o111) == 0 {
                    perms.set_mode(0o755);
                    let _ = fs::set_permissions(&bin, perms);
                }
            }
        }
        push_log(
            &mut rt.logs,
            format!("[Studio] 바이너리 실행: {}", bin.display()),
        );
        Command::new(bin)
    } else if ws.join("Cargo.toml").exists() {
        push_log(
            &mut rt.logs,
            "[Studio] 바이너리가 없어 cargo run 으로 시작합니다...".into(),
        );
        let cargo_cmd = find_cargo_binary().unwrap_or_else(|| PathBuf::from("cargo"));
        let mut c = Command::new(cargo_cmd);
        c.arg("run");
        c
    } else {
        return Err("CRust 실행 파일(rust_board.exe) 또는 Cargo.toml을 찾을 수 없습니다.".into());
    };

    cmd.current_dir(&ws)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let msg = format!("CRust 서버 시작 실패: {e}");
            push_log(&mut rt.logs, format!("[Studio] {msg}"));
            return Err(msg);
        }
    };

    if let Some(stdout) = child.stdout.take() {
        let app_handle = app.clone();
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                if let Ok(mut rt) = app_handle.state::<AppState>().0.lock() {
                    push_log(&mut rt.logs, line);
                }
            }
        });
    }
    if let Some(stderr) = child.stderr.take() {
        let app_handle = app.clone();
        std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                if let Ok(mut rt) = app_handle.state::<AppState>().0.lock() {
                    push_log(&mut rt.logs, line);
                }
            }
        });
    }

    rt.child = Some(child);
    rt.port = port;
    push_log(
        &mut rt.logs,
        format!("[Studio] CRust 서버가 http://127.0.0.1:{} 에서 시작되었습니다.", port),
    );

    if open_browser {
        let url = format!("http://127.0.0.1:{}", port);
        let app_clone = app.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(900));
            let _ = app_clone.opener().open_url(url, None::<&str>);
        });
    }

    Ok(build_status(&mut rt))
}

#[tauri::command]
fn stop_server(state: State<AppState>) -> Result<StudioStatus, String> {
    let mut rt = state.0.lock().map_err(|e| e.to_string())?;
    if let Some(mut child) = rt.child.take() {
        let _ = child.kill();
        let _ = child.wait();
        push_log(&mut rt.logs, "[Studio] CRust 서버가 중지되었습니다.".into());
    }
    Ok(build_status(&mut rt))
}

#[tauri::command]
fn open_in_browser(
    app: tauri::AppHandle,
    state: State<AppState>,
    suffix: String,
) -> Result<(), String> {
    let rt = state.0.lock().map_err(|e| e.to_string())?;
    let port = if rt.port > 0 { rt.port } else { 3000 };
    let clean_suffix = if suffix.starts_with('/') {
        suffix
    } else {
        format!("/{}", suffix)
    };
    let url = format!("http://127.0.0.1:{}{}", port, clean_suffix);
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| format!("브라우저 열기 실패: {e}"))
}

#[tauri::command]
fn open_folder(
    app: tauri::AppHandle,
    state: State<AppState>,
    subpath: String,
) -> Result<(), String> {
    let rt = state.0.lock().map_err(|e| e.to_string())?;
    let ws = rt
        .workspace_path
        .as_ref()
        .ok_or_else(|| "작업 폴더가 설정되지 않았습니다.".to_string())?;
    let target = if subpath.trim().is_empty() {
        ws.clone()
    } else {
        ws.join(subpath.trim())
    };
    if !target.exists() {
        return Err(format!("폴더를 찾을 수 없습니다: {}", target.display()));
    }
    app.opener()
        .open_path(target.to_string_lossy().to_string(), None::<&str>)
        .map_err(|e| format!("폴더 열기 실패: {e}"))
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let dst_path = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&entry.path(), &dst_path)?;
        } else {
            fs::copy(entry.path(), dst_path)?;
        }
    }
    Ok(())
}

#[tauri::command]
fn create_theme(
    state: State<AppState>,
    slug: String,
    display_name: String,
    author: String,
) -> Result<StudioStatus, String> {
    let clean_slug = slug.trim().to_lowercase();
    if clean_slug.is_empty()
        || !clean_slug
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("테마 ID는 영문 소문자, 숫자, 하이픈(-), 언더바(_)만 사용할 수 있습니다.".into());
    }

    let mut rt = state.0.lock().map_err(|e| e.to_string())?;
    let ws = rt
        .workspace_path
        .clone()
        .ok_or_else(|| "작업 폴더가 설정되지 않았습니다.".to_string())?;

    let themes_dir = ws.join("templates").join("themes");
    let src_default = themes_dir.join("default");
    let dst_theme = themes_dir.join(&clean_slug);

    if dst_theme.exists() {
        return Err(format!("이미 존재하는 테마 ID입니다: {}", clean_slug));
    }
    if !src_default.exists() {
        return Err("기준이 될 templates/themes/default 폴더가 없습니다.".into());
    }

    copy_dir_recursive(&src_default, &dst_theme)
        .map_err(|e| format!("테마 복제 실패: {e}"))?;

    let name = if display_name.trim().is_empty() {
        clean_slug.clone()
    } else {
        display_name.trim().to_string()
    };
    let auth = if author.trim().is_empty() {
        "CRust Designer".to_string()
    } else {
        author.trim().to_string()
    };
    let toml = format!(
        "name = \"{}\"\nversion = \"1.0.0\"\nauthor = \"{}\"\ndescription = \"Custom theme created with CRust Studio\"\n",
        name.replace('"', "'"),
        auth.replace('"', "'")
    );
    fs::write(dst_theme.join("theme.toml"), toml)
        .map_err(|e| format!("theme.toml 작성 실패: {e}"))?;

    push_log(
        &mut rt.logs,
        format!("[Studio] 새 테마 생성 완료: templates/themes/{}", clean_slug),
    );
    Ok(build_status(&mut rt))
}

#[tauri::command]
fn export_theme_zip(
    app: tauri::AppHandle,
    state: State<AppState>,
    slug: String,
) -> Result<String, String> {
    let rt = state.0.lock().map_err(|e| e.to_string())?;
    let ws = rt
        .workspace_path
        .as_ref()
        .ok_or_else(|| "작업 폴더가 설정되지 않았습니다.".to_string())?;

    let theme_dir = ws.join("templates").join("themes").join(slug.trim());
    if !theme_dir.is_dir() {
        return Err("테마 폴더를 찾을 수 없습니다.".into());
    }

    let exports_dir = ws.join("exports");
    fs::create_dir_all(&exports_dir).map_err(|e| format!("exports 폴더 생성 실패: {e}"))?;
    let zip_path = exports_dir.join(format!("{}-theme.zip", slug.trim()));

    let file = File::create(&zip_path).map_err(|e| format!("ZIP 파일 생성 실패: {e}"))?;
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    for entry in WalkDir::new(&theme_dir).into_iter().filter_map(Result::ok) {
        let p = entry.path();
        if p.is_file() {
            let rel = p
                .strip_prefix(&theme_dir)
                .map_err(|e| e.to_string())?
                .to_string_lossy()
                .replace('\\', "/");
            let archive_name = format!("{}/{}", slug.trim(), rel);
            zip.start_file(archive_name, options)
                .map_err(|e| format!("ZIP 쓰기 오류: {e}"))?;
            let bytes = fs::read(p).map_err(|e| format!("파일 읽기 오류: {e}"))?;
            std::io::Write::write_all(&mut zip, &bytes)
                .map_err(|e| format!("ZIP 데이터 쓰기 오류: {e}"))?;
        }
    }
    zip.finish().map_err(|e| format!("ZIP 마무리 오류: {e}"))?;

    let _ = app
        .opener()
        .open_path(exports_dir.to_string_lossy().to_string(), None::<&str>);

    Ok(zip_path.to_string_lossy().to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(AppState(Mutex::new(StudioRuntime::default())))
        .invoke_handler(tauri::generate_handler![
            get_studio_state,
            set_workspace_path,
            install_crust,
            toggle_dev_mode,
            start_server,
            stop_server,
            open_in_browser,
            open_folder,
            create_theme,
            export_theme_zip
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|app_handle, event| {
        if let RunEvent::Exit = event {
            if let Ok(mut rt) = app_handle.state::<AppState>().0.lock() {
                if let Some(mut child) = rt.child.take() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        }
    });
}
