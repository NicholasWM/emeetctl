//! Quem está com a câmera aberta: varre `/proc/<pid>/fd` atrás do nó de vídeo.
//!
//! Só enxerga processos do mesmo usuário (os de outros usuários não deixam ler
//! `/proc/<pid>/fd`), o que cobre navegador, Zoom, OBS e afins na sessão gráfica.

use serde::Serialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CameraUser {
    /// Nome amigável do programa (ex.: "Google Chrome").
    pub app: String,
    /// PIDs desse programa com o nó aberto.
    pub pids: Vec<u32>,
}

/// Programas (fora este processo) com `video` aberto, agrupados por nome.
pub fn camera_users(video: &Path) -> Vec<CameraUser> {
    let me = std::process::id();
    let mut by_app: BTreeMap<String, Vec<u32>> = BTreeMap::new();
    let Ok(procs) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    for entry in procs.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == me || !holds(&entry.path(), video) {
            continue;
        }
        let comm = fs::read_to_string(entry.path().join("comm")).unwrap_or_default();
        by_app
            .entry(friendly(comm.trim()).to_string())
            .or_default()
            .push(pid);
    }
    by_app
        .into_iter()
        .map(|(app, pids)| CameraUser { app, pids })
        .collect()
}

fn holds(proc_dir: &Path, video: &Path) -> bool {
    let Ok(fds) = fs::read_dir(proc_dir.join("fd")) else {
        return false;
    };
    fds.flatten()
        .any(|fd| fs::read_link(fd.path()).is_ok_and(|t| t == video))
}

/// "Google Chrome, Zoom" — ou `None` se ninguém está usando.
pub fn describe(users: &[CameraUser]) -> Option<String> {
    (!users.is_empty()).then(|| {
        users
            .iter()
            .map(|u| u.app.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    })
}

/// Nome que a pessoa reconhece, a partir do `comm` do processo.
pub fn friendly(comm: &str) -> &str {
    match comm {
        "chrome" | "google-chrome" => "Google Chrome",
        "chromium" | "chromium-browse" => "Chromium",
        "brave" => "Brave",
        "msedge" => "Microsoft Edge",
        "firefox" | "firefox-bin" | "GeckoMain" => "Firefox",
        "zoom" | "ZoomWebviewHost" => "Zoom",
        "obs" => "OBS Studio",
        "Discord" | "discord" => "Discord",
        "slack" => "Slack",
        "teams" | "teams-for-linux" => "Microsoft Teams",
        "skypeforlinux" => "Skype",
        "cheese" => "Cheese",
        "snapshot" => "Câmera (GNOME)",
        "ffmpeg" | "ffplay" => "ffmpeg",
        "vlc" => "VLC",
        "emeetctl-gui" => "emeetctl",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn friendly_names() {
        assert_eq!(friendly("chrome"), "Google Chrome");
        assert_eq!(friendly("obs"), "OBS Studio");
        assert_eq!(friendly("algo-novo"), "algo-novo");
    }

    #[test]
    fn nobody_holds_a_missing_node() {
        assert!(camera_users(Path::new("/dev/video-inexistente")).is_empty());
    }
}
