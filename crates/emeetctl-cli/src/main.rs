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
    /// Microfone embutido (ganho, mudo, entrada padrão, nível ao vivo)
    #[command(subcommand)]
    Mic(MicCmd),
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
    /// Ganho, mudo, se é a entrada padrão e quem está gravando
    Status,
    /// Ganho em dB (dentro da faixa da câmera, 0–20 dB na S600L)
    Gain {
        #[arg(allow_negative_numbers = true)]
        db: f32,
    },
    /// Silencia o microfone
    Mute,
    /// Liga o microfone
    Unmute,
    /// Torna o microfone da câmera a entrada padrão do sistema
    Default,
    /// Mostra o nível ao vivo (Ctrl+C para parar)
    Level {
        /// Para depois de N segundos
        #[arg(long)]
        seconds: Option<u64>,
    },
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
        Cmd::Mic(cmd) => mic(cmd, cli.json)?,
        Cmd::Image(ImageCmd::Reset) => {
            println!("{} controles no padrão", video()?.reset_defaults()?)
        }
    }
    Ok(())
}

fn mic(cmd: MicCmd, json: bool) -> R {
    let m = Mic::open()?;
    match cmd {
        MicCmd::Status => {}
        MicCmd::Gain { db } => {
            let applied = m.set_gain(db)?;
            if !json {
                println!("ganho: {applied:.1} dB");
                return Ok(());
            }
        }
        MicCmd::Mute => m.set_on(false)?,
        MicCmd::Unmute => m.set_on(true)?,
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
        "microfone: {}\nganho:     {:.1} dB ({:.0}–{:.0} dB)\npadrão:    {}\nem uso:    {users}",
        if s.on { "ligado" } else { "mudo" },
        s.gain_db,
        s.min_db,
        s.max_db,
        if s.default { "sim" } else { "não" },
    );
    Ok(())
}

/// Barra de nível no terminal, redesenhada na mesma linha.
fn level(m: &Mic, seconds: Option<u64>) -> R {
    use emeetctl_core::mic::{fraction, Meter};
    use std::io::Write;
    let node = m
        .status()?
        .pipewire_node
        .ok_or("microfone da câmera não aparece no PipeWire")?;
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
        print!(
            "\r{db:6.1} dBFS |{}{}|",
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
