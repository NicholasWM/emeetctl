//! Transcrição local de uma gravação, com o Whisper (`whisper-ctranslate2`,
//! que roda o faster-whisper na GPU quando tem e na CPU quando não tem).
//!
//! - Cada trilha de áudio é transcrita sozinha e os trechos são intercalados
//!   pelo tempo. Na gravação do PC o microfone e o som do sistema estão em
//!   trilhas separadas ([`crate::record::TRACK_MIC`], [`crate::record::TRACK_SYSTEM`]),
//!   então o texto já sai dizendo quem falou, sem diarização.
//! - O filtro de voz (VAD) do faster-whisper fica ligado: trilha em silêncio
//!   (ninguém falando no sistema) não vira texto inventado.
//! - Saída ao lado da gravação: `.txt` com `[hh:mm:ss] Trilha: texto` e `.srt`.

use crate::{Error, Result};
use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct Options {
    /// Modelo do Whisper (`large-v3`, `medium`, `small`...).
    pub model: String,
    /// Idioma (`pt`, `en`...); sem ele, o Whisper detecta por trilha.
    pub language: Option<String>,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            model: "large-v3".into(),
            language: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    /// Nome da trilha (`Microfone`, `Sistema`) ou `Trilha N`.
    pub track: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Transcript {
    pub txt: PathBuf,
    pub srt: PathBuf,
    pub segments: usize,
}

/// O executável do `whisper-ctranslate2`: no PATH ou em `~/.local/bin` (onde o
/// `uv tool install` e o `pipx` instalam).
pub fn whisper() -> Result<PathBuf> {
    let name = "whisper-ctranslate2";
    let local = std::env::var_os("HOME").map(|h| Path::new(&h).join(".local/bin").join(name));
    std::env::var_os("PATH")
        .iter()
        .flat_map(std::env::split_paths)
        .map(|d| d.join(name))
        .chain(local)
        .find(|p| p.is_file())
        .ok_or_else(|| {
            Error::Invalid(
                "whisper-ctranslate2 não encontrado: uv tool install whisper-ctranslate2".into(),
            )
        })
}

/// Nome de cada trilha de áudio do arquivo, na ordem.
pub fn tracks(path: &Path) -> Result<Vec<String>> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "a",
            "-show_entries",
            "stream_tags=title",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()?;
    if !out.status.success() {
        return Err(Error::Invalid(format!(
            "ffprobe não leu {}: {}",
            path.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let names: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .enumerate()
        .map(|(i, t)| match t.trim() {
            "" => format!("Trilha {}", i + 1),
            t => t.to_string(),
        })
        .collect();
    if names.is_empty() {
        return Err(Error::Invalid(format!("{} não tem áudio", path.display())));
    }
    Ok(names)
}

/// Transcreve `path` e grava o `.txt` e o `.srt` ao lado dele.
pub fn transcribe(path: &Path, o: &Options) -> Result<Transcript> {
    let bin = whisper()?;
    let names = tracks(path)?;
    let tmp = std::env::temp_dir().join(format!("emeetctl-transcribe-{}", std::process::id()));
    std::fs::create_dir_all(&tmp)?;
    let result = (|| -> Result<Vec<Segment>> {
        let mut all = Vec::new();
        for (i, name) in names.iter().enumerate() {
            let wav = tmp.join(format!("trilha{i}.wav"));
            extract(path, i, &wav)?;
            let json = run_whisper(&bin, &wav, &tmp, o)?;
            all.extend(segments(&json, name)?);
        }
        Ok(all)
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    let mut all = result?;
    all.sort_by(|a, b| a.start.total_cmp(&b.start));
    let txt = path.with_extension("txt");
    let srt = path.with_extension("srt");
    std::fs::write(&txt, to_txt(&all))?;
    std::fs::write(&srt, to_srt(&all))?;
    Ok(Transcript {
        txt,
        srt,
        segments: all.len(),
    })
}

/// Uma trilha em WAV mono de 16 kHz, o formato que o Whisper usa.
fn extract(path: &Path, track: usize, wav: &Path) -> Result<()> {
    let st = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(path)
        .args(["-map", &format!("0:a:{track}"), "-ac", "1", "-ar", "16000"])
        .arg(wav)
        .status()?;
    if !st.success() {
        return Err(Error::Invalid(format!(
            "o ffmpeg não extraiu a trilha {} de {}",
            track + 1,
            path.display()
        )));
    }
    Ok(())
}

/// Roda o Whisper e devolve o JSON dele. Tenta a GPU; se ela falhar (sem
/// CUDA, sem memória), refaz na CPU.
fn run_whisper(bin: &Path, wav: &Path, dir: &Path, o: &Options) -> Result<Value> {
    let attempt = |device: &[&str]| {
        let mut c = Command::new(bin);
        c.arg(wav)
            .args(["--model", &o.model])
            .args(["--output_format", "json", "--vad_filter", "True"])
            .arg("--output_dir")
            .arg(dir)
            .args(device);
        if let Some(l) = &o.language {
            c.args(["--language", l]);
        }
        c.output()
    };
    let mut out = attempt(&["--device", "auto", "--compute_type", "auto"])?;
    if !out.status.success() {
        out = attempt(&["--device", "cpu", "--compute_type", "int8"])?;
    }
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let last = err.lines().rfind(|l| !l.trim().is_empty()).unwrap_or("");
        return Err(Error::Invalid(format!("o Whisper falhou: {last}")));
    }
    let json = wav.with_extension("json");
    let json = dir.join(json.file_name().unwrap_or_default());
    let text = std::fs::read_to_string(&json)?;
    serde_json::from_str(&text)
        .map_err(|e| Error::Invalid(format!("JSON do Whisper ilegível: {e}")))
}

fn segments(json: &Value, track: &str) -> Result<Vec<Segment>> {
    let list = json
        .get("segments")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::Invalid("JSON do Whisper sem 'segments'".into()))?;
    Ok(list
        .iter()
        .filter_map(|s| {
            let text = s.get("text")?.as_str()?.trim();
            (!text.is_empty()).then(|| Segment {
                start: s.get("start").and_then(Value::as_f64).unwrap_or(0.0),
                end: s.get("end").and_then(Value::as_f64).unwrap_or(0.0),
                track: track.to_string(),
                text: text.to_string(),
            })
        })
        .collect())
}

fn clock(t: f64, sep: char) -> String {
    let ms = (t.max(0.0) * 1000.0).round() as u64;
    let (h, m, s) = (ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60);
    match sep {
        ',' => format!("{h:02}:{m:02}:{s:02},{:03}", ms % 1000),
        _ => format!("{h:02}:{m:02}:{s:02}"),
    }
}

fn to_txt(all: &[Segment]) -> String {
    all.iter()
        .map(|s| format!("[{}] {}: {}\n", clock(s.start, ':'), s.track, s.text))
        .collect()
}

fn to_srt(all: &[Segment]) -> String {
    all.iter()
        .enumerate()
        .map(|(i, s)| {
            format!(
                "{}\n{} --> {}\n{}: {}\n\n",
                i + 1,
                clock(s.start, ','),
                clock(s.end, ','),
                s.track,
                s.text
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn segments_skip_blank_text() {
        let j = json!({ "segments": [
            { "start": 1.5, "end": 3.0, "text": " Hello there " },
            { "start": 4.0, "end": 4.5, "text": "  " }
        ]});
        let s = segments(&j, "Sistema").unwrap();
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].text, "Hello there");
        assert_eq!(s[0].track, "Sistema");
        assert!(segments(&json!({}), "x").is_err());
    }

    #[test]
    fn tracks_interleave_by_time_in_txt_and_srt() {
        let mut all = vec![
            Segment {
                start: 65.2,
                end: 67.0,
                track: "Microfone".into(),
                text: "Fine, thanks".into(),
            },
            Segment {
                start: 61.0,
                end: 63.9,
                track: "Sistema".into(),
                text: "How are you?".into(),
            },
        ];
        all.sort_by(|a, b| a.start.total_cmp(&b.start));
        assert_eq!(
            to_txt(&all),
            "[00:01:01] Sistema: How are you?\n[00:01:05] Microfone: Fine, thanks\n"
        );
        assert!(to_srt(&all).starts_with(
            "1\n00:01:01,000 --> 00:01:03,900\nSistema: How are you?\n\n2\n00:01:05,200"
        ));
    }

    #[test]
    fn clock_formats() {
        assert_eq!(clock(3725.5, ':'), "01:02:05");
        assert_eq!(clock(0.0421, ','), "00:00:00,042");
    }
}
