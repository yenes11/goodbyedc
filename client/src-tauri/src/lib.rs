mod screen;
mod sfu;
mod update;
mod voice;

use std::sync::Arc;

use libwebrtc::prelude::PeerConnectionFactory;
use serde::Serialize;
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::State;
use tokio::sync::Mutex;

use crate::screen::SourceInfo;
use crate::sfu::Sfu;
use crate::voice::{Call, Participant, Shared};

struct AppState {
    factory: PeerConnectionFactory,
    call: Mutex<Option<Call>>,
    shared: Arc<Shared>,
}

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JoinResult {
    session_id: String,
    tracks: Vec<&'static str>,
}

/// Ses kanalına bağlanır: SFU oturumu açar ve mikrofonu yayınlar.
#[tauri::command]
async fn voice_join(
    state: State<'_, AppState>,
    base_url: String,
    token: String,
    muted: bool,
    deafened: bool,
) -> CmdResult<JoinResult> {
    let mut guard = state.call.lock().await;
    if let Some(old) = guard.take() {
        old.leave();
    }
    let call = Call::join(state.factory.clone(), Sfu::new(&base_url, &token), state.shared.clone(), muted)
        .await
        .map_err(|e| format!("{e:#}"))?;
    call.set_deafened(deafened);
    let res = JoinResult { session_id: call.session_id.clone(), tracks: call.tracks() };
    *guard = Some(call);
    Ok(res)
}

#[tauri::command]
async fn voice_leave(state: State<'_, AppState>) -> CmdResult<()> {
    if let Some(call) = state.call.lock().await.take() {
        call.leave();
    }
    Ok(())
}

/// Kanal üyeleri değiştiğinde çağrılır; kimin sesini/ekranını çekeceğimizi günceller.
#[tauri::command]
async fn voice_sync(
    state: State<'_, AppState>,
    participants: Vec<Participant>,
    watch: Option<String>,
) -> CmdResult<()> {
    if let Some(call) = state.call.lock().await.as_mut() {
        call.sync(&participants, watch.as_deref()).await;
    }
    Ok(())
}

#[tauri::command]
async fn set_muted(state: State<'_, AppState>, muted: bool) -> CmdResult<()> {
    if let Some(call) = state.call.lock().await.as_ref() {
        call.set_muted(muted);
    }
    Ok(())
}

#[tauri::command]
async fn set_deafened(state: State<'_, AppState>, deafened: bool) -> CmdResult<()> {
    if let Some(call) = state.call.lock().await.as_ref() {
        call.set_deafened(deafened);
    }
    Ok(())
}

#[tauri::command]
async fn screen_sources() -> CmdResult<Vec<SourceInfo>> {
    tokio::task::spawn_blocking(screen::list_sources).await.map_err(err)
}

/// Ekran paylaşımını başlatır, güncel track listesini döner.
#[tauri::command]
async fn screen_start(state: State<'_, AppState>, id: u64, kind: String) -> CmdResult<Vec<&'static str>> {
    let mut guard = state.call.lock().await;
    let call = guard.as_mut().ok_or("Önce bir ses kanalına katıl")?;
    call.start_screen(id, kind).await.map_err(|e| format!("{e:#}"))?;
    Ok(call.tracks())
}

#[tauri::command]
async fn screen_stop(state: State<'_, AppState>) -> CmdResult<Vec<&'static str>> {
    let mut guard = state.call.lock().await;
    let Some(call) = guard.as_mut() else { return Ok(vec![]) };
    call.stop_screen().await.map_err(|e| format!("{e:#}"))?;
    Ok(call.tracks())
}

/// Arayüz, izlenen ekranın JPEG karelerini bu kanaldan alır.
#[tauri::command]
fn screen_subscribe(state: State<'_, AppState>, channel: Channel<InvokeResponseBody>) {
    *state.shared.frame_sink.lock() = Some(channel);
}

/// Arayüz, ses seviyelerini (kim konuşuyor) bu kanaldan alır.
#[tauri::command]
fn levels_subscribe(state: State<'_, AppState>, channel: Channel<serde_json::Value>) {
    *state.shared.level_sink.lock() = Some(channel);
}

/// Yeni sürüm var mı? (Geliştirme sürümünde hep "yok" döner.)
#[tauri::command]
async fn update_check() -> CmdResult<update::Check> {
    update::check().await.map_err(|e| format!("{e:#}"))
}

/// Yeni sürümü indirip kurar ve uygulamayı yeniden başlatır.
#[tauri::command]
async fn update_apply(app: tauri::AppHandle, state: State<'_, AppState>) -> CmdResult<()> {
    update::apply().await.map_err(|e| format!("{e:#}"))?;
    if let Some(call) = state.call.lock().await.take() {
        call.leave();
    }
    app.restart();
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("goodbyedc=info")).init();
    update::startup();

    tauri::Builder::default()
        .manage(AppState {
            factory: PeerConnectionFactory::default(),
            call: Mutex::new(None),
            shared: Arc::new(Shared::default()),
        })
        .setup(|_app| {
            // macOS'ta sistem düğmeleri kenar çubuğunun üstüne yerleşir (titleBarStyle: Overlay).
            // Windows/Linux'ta sistem çerçevesi kaldırılır, düğmeleri arayüz çizer.
            #[cfg(not(target_os = "macos"))]
            {
                use tauri::Manager;
                if let Some(w) = _app.get_webview_window("main") {
                    let _ = w.set_decorations(false);
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            voice_join,
            voice_leave,
            voice_sync,
            set_muted,
            set_deafened,
            screen_sources,
            screen_start,
            screen_stop,
            screen_subscribe,
            levels_subscribe,
            update_check,
            update_apply,
        ])
        .run(tauri::generate_context!())
        .expect("uygulama başlatılamadı");
}
