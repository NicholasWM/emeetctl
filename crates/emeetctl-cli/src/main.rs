use clap::{Parser, Subcommand, ValueEnum};
use emeetctl_core::{light, ControlKind, Light, Mic, Mode, Target, Video};
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "emeetctl",
    version,
    about = "Luz, imagem e microfone da EMEET SmartCam S600L no Linux"
)]
struct Cli {
    /// Saída em JSON
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Dados do dispositivo (produto, serial, firmware, nós)
    Info,
    /// Estado da luz
    Status,
    /// Liga a luz (rgb = anel Ambient, fill = luz de preenchimento)
    On {
        #[arg(value_enum, default_value = "rgb")]
        alvo: Alvo,
    },
    /// Desliga a luz
    Off {
        #[arg(value_enum, default_value = "rgb")]
        alvo: Alvo,
    },
    /// Cor do anel RGB, em RRGGBB (qualquer cor)
    Color { rgb: String },
    /// Modo arco-íris
    Rainbow,
    /// Modo de luz
    Mode {
        #[arg(value_enum)]
        modo: ModoArg,
    },
    /// Brilho em % da faixa do modo atual, ou cru com --raw
    Brightness {
        percent: Option<u8>,
        #[arg(long, value_parser = parse_u8)]
        raw: Option<u8>,
    },
    /// Controles de imagem (UVC)
    #[command(subcommand)]
    Image(ImageCmd),
    /// Microfones USB: o da câmera, Yeti Nano e outros (ganho, mudo, fone, entrada padrão, nível)
    Mic {
        /// Qual microfone: placa, apelido (s600l, nano), vid:pid ou trecho do nome.
        /// Sem isso, a entrada padrão do sistema.
        #[arg(short, long)]
        device: Option<String>,
        #[command(subcommand)]
        cmd: MicCmd,
    },
    /// Grava vídeo (câmera + som) ou só áudio, conforme o formato (Ctrl+C para parar)
    Record {
        /// Vídeo: mp4, mkv, webm. Só áudio: wav, flac, mp3, opus, m4a
        #[arg(short, long, default_value = "mp4")]
        format: String,
        /// De onde vem o som: padrao (entrada padrão), limpo, nenhum, ou um
        /// microfone (placa, s600l, nano, vid:pid, trecho do nome)
        #[arg(short, long)]
        mic: Option<String>,
        /// Para depois de N segundos
        #[arg(long)]
        seconds: Option<u32>,
        /// Arquivo de saída (padrão: pasta de vídeos ou de música, com data e hora)
        #[arg(short, long)]
        output: Option<std::path::PathBuf>,
    },
    /// Grava o PC: o som do sistema e o microfone em trilhas separadas (e a tela
    /// com --video), e transcreve ao parar (Ctrl+C para parar)
    Pc {
        /// Grava também a tela inteira (X11) num MKV; sem isso, só áudio num MKA
        #[arg(long)]
        video: bool,
        /// Microfone: padrao (entrada padrão), limpo, nenhum, ou um microfone
        /// (placa, s600l, nano, vid:pid, trecho do nome)
        #[arg(short, long)]
        mic: Option<String>,
        /// Não grava o som do sistema, só o microfone
        #[arg(long)]
        no_system: bool,
        /// Quadros por segundo da tela
        #[arg(long, default_value_t = 15)]
        fps: u32,
        /// Para depois de N segundos
        #[arg(long)]
        seconds: Option<u32>,
        /// Arquivo de saída (padrão: pasta de música ou de vídeos, pc-<data e hora>)
        #[arg(short, long)]
        output: Option<std::path::PathBuf>,
        /// Não transcreve ao parar
        #[arg(long)]
        no_transcribe: bool,
        /// Modelo do Whisper
        #[arg(long, default_value = "large-v3")]
        model: String,
        /// Idioma da fala (pt, en...); sem ele, detecta por trilha
        #[arg(long)]
        lang: Option<String>,
    },
    /// Transcreve uma gravação com o Whisper, trilha por trilha (.txt e .srt ao lado)
    Transcribe {
        arquivo: std::path::PathBuf,
        /// Modelo do Whisper
        #[arg(long, default_value = "large-v3")]
        model: String,
        /// Idioma da fala (pt, en...); sem ele, detecta por trilha
        #[arg(long)]
        lang: Option<String>,
    },
    /// Câmera virtual "emeetctl Camera": republica a S600L em várias saídas, uma por app (Ctrl+C para parar)
    Virtualcam {
        /// Quantas saídas criar ("emeetctl Camera", "emeetctl Camera 2", ...): cada
        /// programa usa uma.
        #[arg(long, default_value_t = emeetctl_core::virtualcam::DEFAULT_OUTPUTS)]
        outputs: usize,
    },
}

#[derive(Subcommand)]
enum ImageCmd {
    /// Lista os controles com faixa e valor atual
    List,
    /// Lê um controle (chave, nome ou 0xID)
    Get { controle: String },
    /// Altera um controle; booleanos aceitam on/off, menus aceitam o nome do item
    Set { controle: String, valor: String },
    /// Volta todos os controles ao padrão de fábrica
    Reset,
}

#[derive(Subcommand)]
enum MicCmd {
    /// Microfones USB conectados
    List,
    /// Ganho, mudo, fone, se é a entrada padrão e quem está gravando
    Status,
    /// Ganho em dB (dentro da faixa do microfone: S600L 0–20, Yeti Nano 0–33)
    Gain {
        #[arg(allow_negative_numbers = true)]
        db: f32,
    },
    /// Silencia o microfone
    Mute,
    /// Liga o microfone
    Unmute,
    /// Volume do fone do microfone em dB, ou mute/unmute (ex.: Yeti Nano)
    Headphones {
        #[arg(allow_negative_numbers = true)]
        value: String,
    },
    /// Torna o microfone a entrada padrão do sistema
    Default,
    /// Microfone limpo: entrada nova com redução de ruído, corte de grave e
    /// cancelamento de eco (WebRTC), por cima do microfone escolhido
    Clean {
        #[arg(value_enum)]
        estado: Clean,
        /// Já usar a entrada limpa como padrão do sistema
        #[arg(long)]
        default: bool,
    },
    /// Mostra o nível ao vivo (Ctrl+C para parar)
    Level {
        /// Para depois de N segundos
        #[arg(long)]
        seconds: Option<u64>,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Clean {
    On,
    Off,
    Status,
}

#[derive(Clone, Copy, ValueEnum)]
enum Alvo {
    Rgb,
    Fill,
}

#[derive(Clone, Copy, ValueEnum)]
enum ModoArg {
    Cool,
    Warm,
    Balanced,
    Ambient,
}

fn parse_u8(s: &str) -> Result<u8, String> {
    let r = match s.strip_prefix("0x") {
        Some(h) => u8::from_str_radix(h, 16),
        None => s.parse(),
    };
    r.map_err(|e| e.to_string())
}

fn target(a: Alvo) -> Target {
    match a {
        Alvo::Rgb => Target::Ambient,
        Alvo::Fill => Target::Fill,
    }
}

type R = Result<(), Box<dyn std::error::Error>>;

fn light() -> Result<Light, Box<dyn std::error::Error>> {
    let paths = emeetctl_core::find()?;
    let p = paths.hidraw.ok_or("canal HID da câmera não encontrado")?;
    Ok(Light::open(&p)?)
}

fn video() -> Result<Video, Box<dyn std::error::Error>> {
    let paths = emeetctl_core::find()?;
    let p = paths.video.ok_or("nó de vídeo da câmera não encontrado")?;
    Ok(Video::open(&p)?)
}

fn opt<T: std::fmt::Display>(v: Option<T>) -> String {
    v.map(|v| v.to_string()).unwrap_or_else(|| "?".into())
}

fn run(cli: Cli) -> R {
    match cli.cmd {
        Cmd::Info => {
            let info = emeetctl_core::find()?.info();
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&info)?);
                return Ok(());
            }
            println!(
                "produto:  {}\nserial:   {}\nfirmware: {}\nhidraw:   {}\nvideo:    {}\nem uso:   {}",
                info.product,
                info.serial,
                info.firmware,
                opt(info.hidraw),
                opt(info.video),
                emeetctl_core::describe_users(&info.in_use_by).unwrap_or_else(|| "ninguém".into())
            );
        }
        Cmd::Status => {
            let s = light()?.status()?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&s)?);
                return Ok(());
            }
            let onoff = |b: Option<bool>| {
                b.map(|b| if b { "ligada" } else { "desligada" })
                    .unwrap_or("?")
            };
            println!("fill light: {}\nrgb:        {}\nmodo:       {}\ncor:        {}\nbrilho:     {}% (raw {})",
                onoff(s.fill_light), onoff(s.ambient), opt(s.mode.map(|m| m.name())),
                opt(s.color), opt(s.brightness_percent), opt(s.brightness_raw));
        }
        Cmd::On { alvo } => light()?.set_power(target(alvo), true)?,
        Cmd::Off { alvo } => light()?.set_power(target(alvo), false)?,
        Cmd::Color { rgb } => light()?.set_color(light::parse_hex(&rgb)?)?,
        Cmd::Rainbow => light()?.set_mode(Mode::Rainbow)?,
        Cmd::Mode { modo } => {
            let m = match modo {
                ModoArg::Cool => Mode::Cool,
                ModoArg::Warm => Mode::Warm,
                ModoArg::Balanced => Mode::Balanced,
                ModoArg::Ambient => Mode::Ambient,
            };
            light()?.set_mode(m)?
        }
        Cmd::Brightness { percent, raw } => {
            let mut l = light()?;
            let (mode, v) = match (percent, raw) {
                (_, Some(raw)) => {
                    let m = l.mode()?;
                    (m, l.set_brightness_raw(m, raw)?)
                }
                (Some(p), None) => l.set_brightness_percent(p)?,
                (None, None) => return Err("informe a porcentagem ou --raw".into()),
            };
            println!(
                "{}: raw 0x{v:02x} ({}%)",
                mode.name(),
                light::raw_to_percent(mode, v)
            );
        }
        Cmd::Image(ImageCmd::List) => {
            let ctrls = video()?.controls()?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&ctrls)?);
                return Ok(());
            }
            for c in ctrls {
                let faixa = match &c.kind {
                    ControlKind::Boolean => "on/off".to_string(),
                    ControlKind::Menu { items } => items
                        .iter()
                        .map(|(i, n)| format!("{i}={n}"))
                        .collect::<Vec<_>>()
                        .join(", "),
                    ControlKind::Integer => format!("{}..{}", c.min, c.max),
                };
                println!(
                    "{:34} {:>6}  [{faixa}] padrão {}{}",
                    c.key,
                    c.value,
                    c.default,
                    if c.inactive {
                        "  (inativo: modo automático ligado)"
                    } else {
                        ""
                    }
                );
            }
        }
        Cmd::Image(ImageCmd::Get { controle }) => {
            let c = video()?.find(&controle)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&c)?);
            } else {
                println!("{}", c.value);
            }
        }
        Cmd::Image(ImageCmd::Set { controle, valor }) => {
            let v = video()?;
            let c = v.find(&controle)?;
            let value = parse_value(&c.kind, &valor)?;
            v.set(c.id, value)?;
            println!("{} = {}", c.key, v.get(c.id)?);
        }
        Cmd::Virtualcam { outputs } => {
            let paths = emeetctl_core::find()?;
            let src = paths.video.ok_or("nó de vídeo da câmera não encontrado")?;
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let s = stop.clone();
            ctrlc_handler(move || s.store(true, std::sync::atomic::Ordering::Relaxed));
            let sinks = emeetctl_core::virtualcam::ensure_devices(outputs)?;
            for (i, sink) in sinks.iter().enumerate() {
                eprintln!(
                    "saída \"{}\" em {}",
                    emeetctl_core::virtualcam::label(i),
                    sink.display()
                );
            }
            eprintln!(
                "câmera virtual: {} saídas ← {} (MJPEG {}x{} → YUYV)",
                sinks.len(),
                src.display(),
                emeetctl_core::virtualcam::WIDTH,
                emeetctl_core::virtualcam::HEIGHT
            );
            let mut last = std::time::Instant::now() - std::time::Duration::from_secs(60);
            emeetctl_core::virtualcam::run(&src, &sinks, &stop, |fps| {
                // um log por minuto: o serviço roda o dia todo
                if last.elapsed() >= std::time::Duration::from_secs(60) {
                    eprintln!("{fps:.0} fps");
                    last = std::time::Instant::now();
                }
            })?;
        }
        Cmd::Mic { device, cmd } => mic(device.as_deref(), cmd, cli.json)?,
        Cmd::Record {
            format,
            mic,
            seconds,
            output,
        } => record(&format, mic.as_deref(), seconds, output, cli.json)?,
        Cmd::Pc {
            video,
            mic,
            no_system,
            fps,
            seconds,
            output,
            no_transcribe,
            model,
            lang,
        } => {
            use emeetctl_core::record::{Audio, PcOptions};
            let o = PcOptions {
                video,
                system: !no_system,
                mic: Audio::parse(mic.as_deref())?,
                output,
                seconds,
                fps,
            };
            let t = (!no_transcribe).then_some(emeetctl_core::transcribe::Options {
                model,
                language: lang,
            });
            pc(&o, t, cli.json)?
        }
        Cmd::Transcribe {
            arquivo,
            model,
            lang,
        } => {
            let o = emeetctl_core::transcribe::Options {
                model,
                language: lang,
            };
            transcribe(&arquivo, &o, cli.json)?
        }
        Cmd::Image(ImageCmd::Reset) => {
            println!("{} controles no padrão", video()?.reset_defaults()?)
        }
    }
    Ok(())
}

fn mic(device: Option<&str>, cmd: MicCmd, json: bool) -> R {
    use emeetctl_core::mic;
    if let MicCmd::List = cmd {
        let mics = mic::list();
        if json {
            println!("{}", serde_json::to_string_pretty(&mics)?);
        }
        for d in mics.iter().filter(|_| !json) {
            let alias = d
                .model
                .alias()
                .map(|a| format!(" ({a})"))
                .unwrap_or_default();
            println!("placa {}  {}  {}{alias}", d.card, d.usb_id, d.label());
        }
        return Ok(());
    }
    if let MicCmd::Clean { estado, default } = cmd {
        return clean(device, estado, default, json);
    }
    let m = Mic::find(device)?;
    match cmd {
        MicCmd::List | MicCmd::Status | MicCmd::Clean { .. } => {}
        MicCmd::Gain { db } => {
            let applied = m.set_gain(db)?;
            if !json {
                println!("ganho: {applied:.1} dB");
                return Ok(());
            }
        }
        MicCmd::Mute => m.set_on(false)?,
        MicCmd::Unmute => m.set_on(true)?,
        MicCmd::Headphones { value } => match value.as_str() {
            "mute" => m.set_headphones_on(false)?,
            "unmute" => m.set_headphones_on(true)?,
            v => {
                let applied = m.set_headphones(v.parse()?)?;
                if !json {
                    println!("fone: {applied:.1} dB");
                    return Ok(());
                }
            }
        },
        MicCmd::Default => m.make_default()?,
        MicCmd::Level { seconds } => return level(&m, seconds),
    }
    let s = m.status()?;
    if json {
        println!("{}", serde_json::to_string_pretty(&s)?);
        return Ok(());
    }
    let users = if s.in_use_by.is_empty() {
        "ninguém".to_string()
    } else {
        s.in_use_by.join(", ")
    };
    println!(
        "{} (placa {}, {})",
        s.device.label(),
        s.device.card,
        s.device.usb_id
    );
    if let Some(h) = &s.headphones {
        println!(
            "fone:      {:.1} dB ({:.0} a {:.0} dB){}",
            h.db,
            h.min_db,
            h.max_db,
            if h.on { "" } else { ", mudo" }
        );
    }
    println!(
        "microfone: {}\nganho:     {:.1} dB ({:.0}–{:.0} dB)\npadrão:    {}\nem uso:    {users}",
        if s.on { "ligado" } else { "mudo" },
        s.gain_db,
        s.min_db,
        s.max_db,
        if s.default { "sim" } else { "não" },
    );
    Ok(())
}

fn clean(device: Option<&str>, estado: Clean, default: bool, json: bool) -> R {
    use emeetctl_core::mic::{self, clean};
    match estado {
        Clean::On => {
            let d = mic::find(device)?;
            clean::enable(&d)?;
            if default {
                clean::make_default()?;
            }
        }
        Clean::Off => clean::disable()?,
        Clean::Status => {}
    }
    let s = clean::status();
    if json {
        println!("{}", serde_json::to_string_pretty(&s)?);
        return Ok(());
    }
    println!(
        "microfone limpo: {}{}\norigem:          {}\npadrão:          {}",
        if s.active { "ligado" } else { "desligado" },
        if s.enabled {
            " (liga com a sessão)"
        } else {
            ""
        },
        s.source.as_deref().unwrap_or("—"),
        if s.default { "sim" } else { "não" },
    );
    Ok(())
}

/// Grava até o Ctrl+C (ou o fim de `seconds`), com o tempo correndo na mesma linha.
fn record(
    format: &str,
    mic: Option<&str>,
    seconds: Option<u32>,
    output: Option<std::path::PathBuf>,
    json: bool,
) -> R {
    use emeetctl_core::record::{Audio, Options, Recorder};
    use std::io::Write;
    let mut o = Options::new(format.parse()?);
    o.audio = Audio::parse(mic)?;
    o.seconds = seconds;
    o.output = output;
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let s = stop.clone();
    ctrlc_handler(move || s.store(true, std::sync::atomic::Ordering::Relaxed));
    let mut rec = Recorder::start(&o)?;
    let st = rec.status();
    let sound = match &st.audio {
        Audio::Default => "entrada padrão".to_string(),
        Audio::Node(n) => n.clone(),
        Audio::None => "sem som".to_string(),
    };
    match &st.video {
        Some(v) => eprintln!("gravando vídeo de {v} + {sound}"),
        None => eprintln!("gravando áudio de {sound}"),
    }
    eprintln!("arquivo: {}", st.path);
    let path = loop {
        if let Some(r) = rec.finished() {
            break r?;
        }
        if stop.load(std::sync::atomic::Ordering::Relaxed) {
            eprintln!();
            break rec.stop()?;
        }
        let t = rec.elapsed().as_secs();
        eprint!("\r● {:02}:{:02}  (Ctrl+C para parar)\x1b[K", t / 60, t % 60);
        std::io::stderr().flush()?;
        std::thread::sleep(std::time::Duration::from_millis(200));
    };
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    if json {
        println!(
            "{}",
            serde_json::json!({ "path": path, "bytes": size, "format": o.format })
        );
    } else {
        eprintln!();
        println!("{} ({:.1} MB)", path.display(), size as f64 / 1e6);
    }
    Ok(())
}

fn pc(
    o: &emeetctl_core::record::PcOptions,
    t: Option<emeetctl_core::transcribe::Options>,
    json: bool,
) -> R {
    use emeetctl_core::record::{Audio, Recorder};
    use std::io::Write;
    // falta o Whisper? avisa agora, não depois de uma hora de gravação
    if t.is_some() {
        emeetctl_core::transcribe::whisper()?;
    }
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let s = stop.clone();
    ctrlc_handler(move || s.store(true, std::sync::atomic::Ordering::Relaxed));
    let mut rec = Recorder::start_pc(o)?;
    let st = rec.status();
    let mut what = Vec::new();
    if let Some(v) = &st.video {
        what.push(format!("tela {v}"));
    }
    if st.system {
        what.push("som do sistema".into());
    }
    match &st.audio {
        Audio::Default => what.push("microfone (entrada padrão)".into()),
        Audio::Node(n) => what.push(format!("microfone {n}")),
        Audio::None => {}
    }
    eprintln!("gravando {}", what.join(" + "));
    eprintln!("arquivo: {}", st.path);
    let path = loop {
        if let Some(r) = rec.finished() {
            break r?;
        }
        if stop.load(std::sync::atomic::Ordering::Relaxed) {
            eprintln!();
            break rec.stop()?;
        }
        let t = rec.elapsed().as_secs();
        eprint!(
            "\r● {:02}:{:02}:{:02}  (Ctrl+C para parar)\x1b[K",
            t / 3600,
            t / 60 % 60,
            t % 60
        );
        std::io::stderr().flush()?;
        std::thread::sleep(std::time::Duration::from_millis(200));
    };
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    if !json {
        eprintln!();
        println!("{} ({:.1} MB)", path.display(), size as f64 / 1e6);
    }
    let transcript = match t {
        Some(t) => {
            eprintln!("transcrevendo com o Whisper ({})...", t.model);
            Some(emeetctl_core::transcribe::transcribe(&path, &t)?)
        }
        None => None,
    };
    if json {
        println!(
            "{}",
            serde_json::json!({ "path": path, "bytes": size, "format": o.format(), "transcript": transcript })
        );
    } else if let Some(tr) = transcript {
        println!("{} ({} trechos)", tr.txt.display(), tr.segments);
    }
    Ok(())
}

fn transcribe(path: &std::path::Path, o: &emeetctl_core::transcribe::Options, json: bool) -> R {
    eprintln!("transcrevendo com o Whisper ({})...", o.model);
    let tr = emeetctl_core::transcribe::transcribe(path, o)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&tr)?);
    } else {
        println!("{} ({} trechos)", tr.txt.display(), tr.segments);
        println!("{}", tr.srt.display());
    }
    Ok(())
}

/// Barra de nível no terminal, redesenhada na mesma linha.
fn level(m: &Mic, seconds: Option<u64>) -> R {
    use emeetctl_core::mic::{fraction, Meter};
    use std::io::Write;
    let node = m
        .status()?
        .pipewire_node
        .ok_or("o microfone não aparece no PipeWire")?;
    let meter = Meter::start(&node, || {})?;
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let s = stop.clone();
    ctrlc_handler(move || s.store(true, std::sync::atomic::Ordering::Relaxed));
    let start = std::time::Instant::now();
    const WIDTH: usize = 40;
    while !stop.load(std::sync::atomic::Ordering::Relaxed)
        && seconds.is_none_or(|n| start.elapsed().as_secs() < n)
    {
        let db = meter.peak_db();
        let n = (fraction(db) * WIDTH as f32).round() as usize;
        let hint = match (meter.digital_silence(), m.device().model.mute_button()) {
            (true, Some(b)) => format!("  sinal zerado: {b}"),
            (true, None) => "  sinal zerado: mudo no próprio microfone?".into(),
            _ => String::new(),
        };
        print!(
            "\r{db:6.1} dBFS |{}{}|{hint}\x1b[K",
            "#".repeat(n),
            " ".repeat(WIDTH - n)
        );
        std::io::stdout().flush()?;
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    println!();
    Ok(())
}

fn parse_value(kind: &ControlKind, s: &str) -> Result<i32, Box<dyn std::error::Error>> {
    match (kind, s.to_ascii_lowercase().as_str()) {
        (ControlKind::Boolean, "on" | "true" | "auto") => Ok(1),
        (ControlKind::Boolean, "off" | "false" | "manual") => Ok(0),
        (ControlKind::Menu { items }, name) if name.parse::<i32>().is_err() => items
            .iter()
            .find(|(_, n)| n.to_ascii_lowercase().contains(name))
            .map(|(i, _)| *i)
            .ok_or_else(|| format!("opção '{s}' não existe").into()),
        _ => Ok(s.parse()?),
    }
}

/// SIGINT/SIGTERM viram uma parada limpa (o stream é fechado e a câmera liberada).
fn ctrlc_handler(f: impl Fn() + Send + Sync + 'static) {
    use std::sync::OnceLock;
    static HANDLER: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();
    let _ = HANDLER.set(Box::new(f));
    extern "C" fn on_signal(_: libc::c_int) {
        if let Some(h) = HANDLER.get() {
            h();
        }
    }
    unsafe {
        libc::signal(libc::SIGINT, on_signal as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_signal as libc::sighandler_t);
    }
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("erro: {e}");
            ExitCode::FAILURE
        }
    }
}
