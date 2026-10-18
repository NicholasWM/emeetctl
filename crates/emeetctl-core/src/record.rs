//! Gravação de vídeo (câmera + microfone) ou só de áudio, num arquivo.
//!
//! Quem grava é o `ffmpeg`, num processo filho: codificar H.264/VP9/Opus aqui
//! dentro seria reescrever o que ele já faz bem. O emeetctl escolhe as fontes e
//! os parâmetros e para a gravação do jeito certo (`q` na entrada padrão), para
//! o MP4 sair fechado e tocável.
//!
//! - Vídeo: com a câmera virtual ligada, lê de uma saída livre dela (YUYV), e a
//!   câmera continua disponível para o Meet e para a prévia. Sem ela, abre a
//!   S600L direto em MJPEG 1280×720 a 30 fps, o formato em que o zoom funciona.
//! - Áudio: qualquer entrada do PipeWire pelo nome do nó, via pipewire-pulse
//!   (a padrão do sistema, um microfone USB ou a entrada limpa).
//! - Gravação do PC ([`PcOptions`]): o som que sai do computador (monitor da
//!   saída padrão) e o microfone, cada um na sua trilha, e a tela inteira
//!   (X11) se pedir vídeo. Trilhas separadas deixam a transcrição dizer quem
//!   falou ([`crate::transcribe`]).

use crate::mic::{self, clean, PipeWire};
use crate::virtualcam;
use crate::{Error, Result};
use serde::Serialize;
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Mp4,
    Mkv,
    Webm,
    Wav,
    Flac,
    Mp3,
    Opus,
    M4a,
    /// Matroska só de áudio: várias trilhas Opus (gravação do PC).
    Mka,
}

impl Format {
    pub const VIDEO: [Format; 3] = [Format::Mp4, Format::Mkv, Format::Webm];
    pub const AUDIO: [Format; 5] = [
        Format::Wav,
        Format::Flac,
        Format::Mp3,
        Format::Opus,
        Format::M4a,
    ];

    pub fn ext(self) -> &'static str {
        match self {
            Format::Mp4 => "mp4",
            Format::Mkv => "mkv",
            Format::Webm => "webm",
            Format::Wav => "wav",
            Format::Flac => "flac",
            Format::Mp3 => "mp3",
            Format::Opus => "opus",
            Format::M4a => "m4a",
            Format::Mka => "mka",
        }
    }

    pub fn has_video(self) -> bool {
        Format::VIDEO.contains(&self)
    }

    /// Nome curto para a interface.
    pub fn name(self) -> &'static str {
        match self {
            Format::Mp4 => "MP4",
            Format::Mkv => "MKV",
            Format::Webm => "WebM",
            Format::Wav => "WAV",
            Format::Flac => "FLAC",
            Format::Mp3 => "MP3",
            Format::Opus => "Opus",
            Format::M4a => "M4A",
            Format::Mka => "MKA",
        }
    }

    /// O que vai dentro do arquivo e quando escolher.
    pub fn hint(self) -> &'static str {
        match self {
            Format::Mp4 => "H.264 + AAC: abre em qualquer lugar (celular, WhatsApp, editores)",
            Format::Mkv => "H.264 + Opus: aguenta queda de energia no meio da gravação",
            Format::Webm => "VP9 + Opus: formato aberto da web, arquivo menor, usa mais CPU",
            Format::Wav => "PCM sem compressão: qualidade máxima, arquivo grande",
            Format::Flac => "sem perda, cerca de metade do WAV",
            Format::Mp3 => "compatível com tudo, ~190 kb/s",
            Format::Opus => "o melhor para voz, arquivo pequeno (128 kb/s)",
            Format::M4a => "AAC: o padrão de áudio do iPhone e do Mac",
            Format::Mka => "Opus em trilhas separadas (sistema e microfone)",
        }
    }

    /// Parâmetros de codificação do ffmpeg.
    fn codec_args(self) -> &'static [&'static str] {
        match self {
            Format::Mp4 => &[
                "-c:v",
                "libx264",
                "-preset",
                "veryfast",
                "-crf",
                "23",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-b:a",
                "160k",
                "-movflags",
                "+faststart",
            ],
            Format::Mkv => &[
                "-c:v", "libx264", "-preset", "veryfast", "-crf", "23", "-pix_fmt", "yuv420p",
                "-c:a", "libopus", "-b:a", "128k",
            ],
            Format::Webm => &[
                "-c:v",
                "libvpx-vp9",
                "-deadline",
                "realtime",
                "-cpu-used",
                "8",
                "-row-mt",
                "1",
                "-b:v",
                "0",
                "-crf",
                "32",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "libopus",
                "-b:a",
                "128k",
            ],
            Format::Wav => &["-c:a", "pcm_s16le"],
            Format::Flac => &["-c:a", "flac"],
            Format::Mp3 => &["-c:a", "libmp3lame", "-q:a", "2"],
            Format::Opus => &["-c:a", "libopus", "-b:a", "128k"],
            Format::M4a => &["-c:a", "aac", "-b:a", "160k", "-movflags", "+faststart"],
            Format::Mka => &["-c:a", "libopus", "-b:a", "96k"],
        }
    }
}

impl std::fmt::Display for Format {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.ext())
    }
}

impl std::str::FromStr for Format {
    type Err = Error;
    fn from_str(s: &str) -> Result<Format> {
        let s = s.trim().trim_start_matches('.').to_lowercase();
        Format::VIDEO
            .into_iter()
            .chain(Format::AUDIO)
            .find(|f| f.ext() == s)
            .ok_or_else(|| {
                Error::Invalid(format!(
                    "formato '{s}' não existe (vídeo: mp4, mkv, webm; áudio: wav, flac, mp3, opus, m4a)"
                ))
            })
    }
}

/// De onde vem o som.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase", tag = "kind", content = "node")]
pub enum Audio {
    /// A entrada padrão do sistema na hora de começar.
    Default,
    /// Um nó do PipeWire pelo `node.name`.
    Node(String),
    /// Vídeo mudo.
    None,
}

impl Audio {
    /// Interpreta a escolha do usuário: `padrao`, `limpo`, `nenhum` ou um
    /// microfone (placa, apelido, vid:pid ou trecho do nome).
    pub fn parse(sel: Option<&str>) -> Result<Audio> {
        let Some(s) = sel.map(|s| s.trim().to_lowercase()) else {
            return Ok(Audio::Default);
        };
        match s.as_str() {
            "" | "padrao" | "padrão" | "default" => Ok(Audio::Default),
            "nenhum" | "none" | "sem" | "mudo" => Ok(Audio::None),
            "limpo" | "clean" => {
                if !clean::status().active {
                    return Err(Error::Invalid(
                        "o microfone limpo está desligado (emeetctl mic clean on)".into(),
                    ));
                }
                Ok(Audio::Node(clean::NODE.into()))
            }
            _ => {
                let d = mic::find(Some(&s))?;
                let (_, node) = PipeWire::read()?.source_for_card(d.card).ok_or_else(|| {
                    Error::Invalid(format!("{} não aparece no PipeWire", d.label()))
                })?;
                Ok(Audio::Node(node))
            }
        }
    }

    fn pulse_name(&self) -> Option<&str> {
        match self {
            Audio::Default => Some("default"),
            Audio::Node(n) => Some(n),
            Audio::None => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    pub format: Format,
    pub audio: Audio,
    /// Arquivo de saída; sem ele, `~/Vídeos` ou `~/Música` com data e hora no nome.
    pub output: Option<PathBuf>,
    /// Para sozinho depois de N segundos.
    pub seconds: Option<u32>,
    /// Nó de vídeo que não pode ser usado (a prévia de quem chama já está nele).
    pub avoid: Option<PathBuf>,
}

impl Options {
    pub fn new(format: Format) -> Options {
        Options {
            format,
            audio: Audio::Default,
            output: None,
            seconds: None,
            avoid: None,
        }
    }
}

/// Estado de uma gravação, para JSON (CLI e MCP).
#[derive(Debug, Clone, Serialize)]
pub struct RecordingStatus {
    pub path: String,
    pub format: Format,
    pub seconds: f64,
    /// Nó de vídeo lido (`/dev/videoN`) ou a tela (`:1`), se for vídeo.
    pub video: Option<String>,
    pub audio: Audio,
    /// Grava também o som que sai do computador (gravação do PC).
    pub system: bool,
}

/// Gravação do PC: o som do sistema e o microfone, e a tela se `video`.
#[derive(Debug, Clone)]
pub struct PcOptions {
    /// Grava a tela inteira (X11) num MKV; sem isso, só áudio num MKA.
    pub video: bool,
    /// O som que sai do computador (chamada, vídeo, música).
    pub system: bool,
    /// O microfone; `Audio::None` deixa só o sistema.
    pub mic: Audio,
    pub output: Option<PathBuf>,
    pub seconds: Option<u32>,
    /// Quadros por segundo da tela.
    pub fps: u32,
}

impl Default for PcOptions {
    fn default() -> PcOptions {
        PcOptions {
            video: false,
            system: true,
            mic: Audio::Default,
            output: None,
            seconds: None,
            fps: 15,
        }
    }
}

impl PcOptions {
    pub fn format(&self) -> Format {
        if self.video {
            Format::Mkv
        } else {
            Format::Mka
        }
    }
}

/// Nome da trilha de áudio, na ordem em que entram no arquivo: o microfone
/// primeiro, depois o sistema. A transcrição usa esse nome para dizer quem falou.
pub const TRACK_MIC: &str = "Microfone";
pub const TRACK_SYSTEM: &str = "Sistema";

/// A tela do X11 (`$DISPLAY`). No Wayland o x11grab só vê janelas do XWayland.
pub fn screen() -> Result<String> {
    if std::env::var("XDG_SESSION_TYPE").is_ok_and(|t| t == "wayland") {
        return Err(Error::Invalid(
            "gravar a tela precisa de uma sessão X11 (no Wayland, só áudio)".into(),
        ));
    }
    std::env::var("DISPLAY")
        .ok()
        .filter(|d| !d.is_empty())
        .ok_or_else(|| Error::Invalid("sem $DISPLAY: não há tela X11 para gravar".into()))
}

/// Nó de vídeo para gravar. Com a câmera virtual ligada, a primeira saída livre,
/// na ordem: as do meio (feitas para gravar), a principal ("emeetctl Camera", a
/// das chamadas) e por último a da prévia. Sem ela, a câmera física.
pub fn video_source(avoid: Option<&Path>) -> Result<PathBuf> {
    if virtualcam::service_active() {
        let free = |p: &Path| Some(p) != avoid && virtualcam::readers(p).is_empty();
        let outs = virtualcam::find_devices();
        let n = outs.len();
        let mut order: Vec<usize> = (1..n.saturating_sub(1)).collect();
        order.push(0);
        if n > 1 {
            order.push(n - 1);
        }
        return order
            .into_iter()
            .filter_map(|i| outs.get(i))
            .find(|p| free(p))
            .cloned()
            .ok_or_else(|| {
                Error::Invalid("todas as saídas da câmera virtual estão em uso".into())
            });
    }
    let cam = crate::find()?.video.ok_or(Error::NotFound("nó de vídeo"))?;
    if Some(cam.as_path()) == avoid {
        return Err(Error::Invalid(
            "a prévia está usando a câmera: pause a prévia ou ligue a câmera virtual".into(),
        ));
    }
    match crate::describe_users(&crate::camera_users(&cam)) {
        Some(who) => Err(Error::Invalid(format!(
            "a câmera está em uso por {who}: ligue a câmera virtual para gravar ao mesmo tempo"
        ))),
        None => Ok(cam),
    }
}

/// Pasta padrão: a de vídeos ou a de música do usuário (XDG), criada se faltar.
pub fn default_dir(video: bool) -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let kind = if video { "VIDEOS" } else { "MUSIC" };
    let xdg = Command::new("xdg-user-dir")
        .arg(kind)
        .output()
        .ok()
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()))
        // sem a pasta configurada, o xdg-user-dir devolve a própria home
        .filter(|p| p.is_absolute() && *p != home);
    xdg.unwrap_or_else(|| home.join(if video { "Videos" } else { "Music" }))
}

/// `emeetctl-2026-10-17_23-41-05.mp4`, na hora local.
fn file_name(format: Format) -> String {
    stamped("emeetctl", format)
}

fn stamped(prefix: &str, format: Format) -> String {
    let now = unsafe { libc::time(std::ptr::null_mut()) };
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&now, &mut tm) };
    format!(
        "{prefix}-{:04}-{:02}-{:02}_{:02}-{:02}-{:02}.{}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec,
        format.ext()
    )
}

/// Linha de comando do ffmpeg (sem o binário).
fn ffmpeg_args(o: &Options, video: Option<(&Path, bool)>, path: &Path) -> Vec<String> {
    let mut a: Vec<String> = ["-hide_banner", "-loglevel", "error", "-nostats", "-n"]
        .map(String::from)
        .to_vec();
    if let Some((dev, is_virtual)) = video {
        a.extend(["-thread_queue_size", "512", "-f", "v4l2"].map(String::from));
        if !is_virtual {
            // a câmera física: MJPEG, o formato em que o zoom vale
            a.extend(["-input_format", "mjpeg"].map(String::from));
        }
        a.extend([
            "-video_size".into(),
            format!("{}x{}", virtualcam::WIDTH, virtualcam::HEIGHT),
            "-framerate".into(),
            virtualcam::FPS.to_string(),
            // relógio de parede nos dois lados: é o que o pulse usa, e assim
            // som e imagem saem alinhados
            "-use_wallclock_as_timestamps".into(),
            "1".into(),
            "-i".into(),
            dev.display().to_string(),
        ]);
    }
    if let Some(src) = o.audio.pulse_name() {
        a.extend(
            [
                "-thread_queue_size",
                "512",
                "-f",
                "pulse",
                "-name",
                "emeetctl",
                "-stream_name",
                "Gravação",
                "-i",
                src,
            ]
            .map(String::from),
        );
    }
    if let Some(s) = o.seconds {
        a.extend(["-t".into(), s.to_string()]);
    }
    a.extend(o.format.codec_args().iter().map(|s| s.to_string()));
    a.push(path.display().to_string());
    a
}

/// Linha de comando do ffmpeg da gravação do PC. `screen` é a tela do X11 e
/// `h264` o codificador do vídeo (`h264_nvenc` na GPU NVIDIA, ou `libx264`).
fn pc_args(o: &PcOptions, screen: Option<&str>, h264: &str, path: &Path) -> Vec<String> {
    let mut a: Vec<String> = ["-hide_banner", "-loglevel", "error", "-nostats", "-n"]
        .map(String::from)
        .to_vec();
    if let Some(display) = screen {
        a.extend([
            "-thread_queue_size".into(),
            "512".into(),
            "-f".into(),
            "x11grab".into(),
            "-framerate".into(),
            o.fps.to_string(),
            "-draw_mouse".into(),
            "1".into(),
            "-i".into(),
            display.into(),
        ]);
    }
    let mut tracks = Vec::new();
    if let Some(src) = o.mic.pulse_name() {
        tracks.push((src, TRACK_MIC));
    }
    if o.system {
        // o monitor da saída padrão: tudo o que o computador está tocando
        tracks.push(("@DEFAULT_MONITOR@", TRACK_SYSTEM));
    }
    for (src, title) in &tracks {
        a.extend(
            [
                "-thread_queue_size",
                "512",
                "-f",
                "pulse",
                "-name",
                "emeetctl",
                "-stream_name",
                title,
                "-i",
                src,
            ]
            .map(String::from),
        );
    }
    let first_audio = usize::from(screen.is_some());
    if screen.is_some() {
        a.extend(["-map", "0:v"].map(String::from));
    }
    for i in 0..tracks.len() {
        a.extend(["-map".into(), format!("{}:a", first_audio + i)]);
    }
    for (i, (_, title)) in tracks.iter().enumerate() {
        a.extend([format!("-metadata:s:a:{i}"), format!("title={title}")]);
    }
    if let Some(s) = o.seconds {
        a.extend(["-t".into(), s.to_string()]);
    }
    if screen.is_some() {
        a.extend(["-c:v".into(), h264.into()]);
        a.extend(
            match h264 {
                "h264_nvenc" => ["-preset", "p4", "-cq", "30"],
                _ => ["-preset", "veryfast", "-crf", "28"],
            }
            .map(String::from),
        );
        a.extend(["-pix_fmt", "yuv420p"].map(String::from));
    }
    a.extend(Format::Mka.codec_args().iter().map(|s| s.to_string()));
    a.push(path.display().to_string());
    a
}

pub struct Recorder {
    child: Child,
    stdin: Option<ChildStdin>,
    path: PathBuf,
    format: Format,
    audio: Audio,
    /// Nó de vídeo (`/dev/videoN`) ou tela (`:1`).
    video: Option<String>,
    system: bool,
    started: Instant,
    stderr: Arc<Mutex<String>>,
    done: bool,
}

impl Recorder {
    /// Começa a gravar. Volta com erro se o ffmpeg não conseguir abrir as fontes.
    pub fn start(o: &Options) -> Result<Recorder> {
        if !o.format.has_video() && o.audio == Audio::None {
            return Err(Error::Invalid("gravar só áudio sem nenhum áudio?".into()));
        }
        let video = match o.format.has_video() {
            true => Some(video_source(o.avoid.as_deref())?),
            false => None,
        };
        let is_virtual = video
            .as_ref()
            .is_some_and(|v| virtualcam::find_devices().contains(v));
        let path = match &o.output {
            Some(p) => p.clone(),
            None => {
                let dir = default_dir(o.format.has_video());
                std::fs::create_dir_all(&dir)?;
                dir.join(file_name(o.format))
            }
        };
        let args = ffmpeg_args(o, video.as_deref().map(|v| (v, is_virtual)), &path);
        Recorder::spawn(
            &args,
            path,
            o.format,
            o.audio.clone(),
            video.map(|v| v.display().to_string()),
            false,
        )
    }

    /// Começa a gravar o PC: o som do sistema e o microfone em trilhas
    /// separadas, e a tela se `o.video`. O vídeo sai pela GPU (NVENC) quando dá.
    pub fn start_pc(o: &PcOptions) -> Result<Recorder> {
        if !o.system && o.mic == Audio::None {
            return Err(Error::Invalid(
                "sem som do sistema e sem microfone não há o que gravar".into(),
            ));
        }
        let screen = match o.video {
            true => Some(screen()?),
            false => None,
        };
        let format = o.format();
        let path = match &o.output {
            Some(p) => p.clone(),
            None => {
                let dir = default_dir(o.video);
                std::fs::create_dir_all(&dir)?;
                dir.join(stamped("pc", format))
            }
        };
        if path.exists() {
            return Err(Error::Invalid(format!("{} já existe", path.display())));
        }
        let start = |h264: &str| {
            Recorder::spawn(
                &pc_args(o, screen.as_deref(), h264, &path),
                path.clone(),
                format,
                o.mic.clone(),
                screen.clone(),
                o.system,
            )
        };
        match start("h264_nvenc") {
            // sem GPU NVIDIA (ou sem NVENC no ffmpeg): codifica na CPU
            Err(_) if screen.is_some() => {
                // o ffmpeg abre o arquivo antes de iniciar o codificador
                let _ = std::fs::remove_file(&path);
                start("libx264")
            }
            r => r,
        }
    }

    fn spawn(
        args: &[String],
        path: PathBuf,
        format: Format,
        audio: Audio,
        video: Option<String>,
        system: bool,
    ) -> Result<Recorder> {
        if path.exists() {
            return Err(Error::Invalid(format!("{} já existe", path.display())));
        }
        let mut child = Command::new("ffmpeg")
            .args(args)
            // grupo próprio: o Ctrl+C do terminal não chega nele, quem para é o `q`
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => {
                    Error::Invalid("ffmpeg não encontrado: sudo apt install ffmpeg".into())
                }
                _ => Error::Io(e),
            })?;
        let stderr = Arc::new(Mutex::new(String::new()));
        if let Some(mut err) = child.stderr.take() {
            let buf = stderr.clone();
            std::thread::spawn(move || {
                let mut s = String::new();
                let _ = err.read_to_string(&mut s);
                *buf.lock().unwrap() = s;
            });
        }
        let mut rec = Recorder {
            stdin: child.stdin.take(),
            child,
            path,
            format,
            audio,
            video,
            system,
            started: Instant::now(),
            stderr,
            done: false,
        };
        // Câmera ocupada, nó inexistente, arquivo que já existe: o ffmpeg cai logo.
        let deadline = Instant::now() + Duration::from_millis(600);
        while Instant::now() < deadline {
            if let Some(r) = rec.finished() {
                r?;
                return Err(Error::Invalid("o ffmpeg terminou logo ao começar".into()));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok(rec)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn format(&self) -> Format {
        self.format
    }

    /// Nó de vídeo que a gravação está lendo (ou a tela, na gravação do PC).
    pub fn video(&self) -> Option<&Path> {
        self.video.as_deref().map(Path::new)
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    pub fn status(&self) -> RecordingStatus {
        RecordingStatus {
            path: self.path.display().to_string(),
            format: self.format,
            seconds: self.elapsed().as_secs_f64(),
            video: self.video.clone(),
            audio: self.audio.clone(),
            system: self.system,
        }
    }

    /// Terminou sozinha (fim do tempo ou erro)? `None` enquanto grava.
    pub fn finished(&mut self) -> Option<Result<PathBuf>> {
        if self.done {
            return None;
        }
        let status = self.child.try_wait().ok()??;
        self.done = true;
        Some(self.outcome(status))
    }

    fn outcome(&self, status: std::process::ExitStatus) -> Result<PathBuf> {
        if status.success() {
            return Ok(self.path.clone());
        }
        // o leitor de stderr termina junto com o processo
        std::thread::sleep(Duration::from_millis(50));
        let err = self.stderr.lock().unwrap();
        let last = err.lines().rfind(|l| !l.trim().is_empty()).unwrap_or("");
        Err(Error::Invalid(format!("a gravação falhou: ffmpeg: {last}")))
    }

    /// Para e fecha o arquivo. Devolve o caminho dele.
    pub fn stop(mut self) -> Result<PathBuf> {
        self.finish()
    }

    fn finish(&mut self) -> Result<PathBuf> {
        if self.done {
            return Ok(self.path.clone());
        }
        // `q` é o "parar" do ffmpeg: ele grava o índice e fecha o contêiner.
        if let Some(mut s) = self.stdin.take() {
            let _ = s.write_all(b"q");
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(st) = self.child.try_wait()? {
                break st;
            }
            if Instant::now() > deadline {
                let _ = self.child.kill();
                break self.child.wait()?;
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        self.done = true;
        self.outcome(status)
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_round_trip() {
        for f in Format::VIDEO.into_iter().chain(Format::AUDIO) {
            assert_eq!(f.ext().parse::<Format>().unwrap(), f);
            assert_eq!(f.has_video(), Format::VIDEO.contains(&f));
        }
        assert_eq!(".MP4".parse::<Format>().unwrap(), Format::Mp4);
        assert!("avi".parse::<Format>().is_err());
    }

    #[test]
    fn video_from_the_virtual_camera_has_no_mjpeg_flag() {
        let mut o = Options::new(Format::Mp4);
        o.seconds = Some(5);
        let a = ffmpeg_args(
            &o,
            Some((Path::new("/dev/video4"), true)),
            Path::new("/x.mp4"),
        );
        let s = a.join(" ");
        assert!(!s.contains("mjpeg"));
        assert!(s.contains("-f v4l2 -video_size 1280x720 -framerate 30"));
        assert!(s.contains("-f pulse -name emeetctl -stream_name Gravação -i default"));
        assert!(s.contains("-t 5 -c:v libx264"));
        assert!(s.ends_with("+faststart /x.mp4"));

        let cam = ffmpeg_args(
            &o,
            Some((Path::new("/dev/video1"), false)),
            Path::new("/x.mp4"),
        );
        assert!(cam.join(" ").contains("-input_format mjpeg"));
    }

    #[test]
    fn audio_only_and_mute_video() {
        let mut o = Options::new(Format::Opus);
        o.audio = Audio::Node("emeetctl-clean".into());
        let a = ffmpeg_args(&o, None, Path::new("/x.opus")).join(" ");
        assert!(!a.contains("v4l2"));
        assert!(a.contains("-i emeetctl-clean -c:a libopus"));

        let mut o = Options::new(Format::Webm);
        o.audio = Audio::None;
        let a = ffmpeg_args(
            &o,
            Some((Path::new("/dev/video4"), true)),
            Path::new("/x.webm"),
        );
        assert!(!a.join(" ").contains("pulse"));
    }

    #[test]
    fn pc_audio_only_puts_mic_and_system_in_separate_tracks() {
        let o = PcOptions::default();
        assert_eq!(o.format(), Format::Mka);
        let a = pc_args(&o, None, "h264_nvenc", Path::new("/x.mka")).join(" ");
        assert!(!a.contains("x11grab"));
        assert!(a.contains("-stream_name Microfone -i default"));
        assert!(a.contains("-stream_name Sistema -i @DEFAULT_MONITOR@"));
        assert!(a.contains("-map 0:a -map 1:a"));
        assert!(a.contains("-metadata:s:a:0 title=Microfone -metadata:s:a:1 title=Sistema"));
        assert!(!a.contains("-c:v"));
        assert!(a.ends_with("-c:a libopus -b:a 96k /x.mka"));
    }

    #[test]
    fn pc_with_screen_maps_video_first() {
        let o = PcOptions {
            video: true,
            mic: Audio::None,
            seconds: Some(3),
            ..PcOptions::default()
        };
        assert_eq!(o.format(), Format::Mkv);
        let a = pc_args(&o, Some(":1"), "h264_nvenc", Path::new("/x.mkv")).join(" ");
        assert!(a.contains("-f x11grab -framerate 15 -draw_mouse 1 -i :1"));
        assert!(a.contains("-map 0:v -map 1:a -metadata:s:a:0 title=Sistema"));
        assert!(a.contains("-t 3 -c:v h264_nvenc -preset p4 -cq 30"));
        let cpu = pc_args(&o, Some(":1"), "libx264", Path::new("/x.mkv")).join(" ");
        assert!(cpu.contains("-c:v libx264 -preset veryfast -crf 28"));
    }

    #[test]
    fn file_names_carry_the_extension() {
        let n = file_name(Format::Flac);
        assert!(n.starts_with("emeetctl-20") && n.ends_with(".flac"), "{n}");
        assert_eq!(n.len(), "emeetctl-2026-10-17_23-41-05.flac".len());
    }
}
