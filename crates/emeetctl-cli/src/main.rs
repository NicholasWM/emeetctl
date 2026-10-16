use clap::{Parser, Subcommand, ValueEnum};
use emeetctl_core::{light, ControlKind, Light, Mode, Target, Video};
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "emeetctl", version, about = "Luz e imagem da EMEET SmartCam S600L no Linux")]
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
    On { #[arg(value_enum, default_value = "rgb")] alvo: Alvo },
    /// Desliga a luz
    Off { #[arg(value_enum, default_value = "rgb")] alvo: Alvo },
    /// Cor do anel RGB, em RRGGBB (qualquer cor)
    Color { rgb: String },
    /// Modo arco-íris
    Rainbow,
    /// Modo de luz
    Mode { #[arg(value_enum)] modo: ModoArg },
    /// Brilho em % da faixa do modo atual, ou cru com --raw
    Brightness {
        percent: Option<u8>,
        #[arg(long, value_parser = parse_u8)]
        raw: Option<u8>,
    },
    /// Controles de imagem (UVC)
    #[command(subcommand)]
    Image(ImageCmd),
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

#[derive(Clone, Copy, ValueEnum)]
enum Alvo { Rgb, Fill }

#[derive(Clone, Copy, ValueEnum)]
enum ModoArg { Cool, Warm, Balanced, Ambient }

fn parse_u8(s: &str) -> Result<u8, String> {
    let r = match s.strip_prefix("0x") {
        Some(h) => u8::from_str_radix(h, 16),
        None => s.parse(),
    };
    r.map_err(|e| e.to_string())
}

fn target(a: Alvo) -> Target {
    match a { Alvo::Rgb => Target::Ambient, Alvo::Fill => Target::Fill }
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
            if cli.json { println!("{}", serde_json::to_string_pretty(&info)?); return Ok(()); }
            println!("produto:  {}\nserial:   {}\nfirmware: {}\nhidraw:   {}\nvideo:    {}",
                info.product, info.serial, info.firmware, opt(info.hidraw), opt(info.video));
        }
        Cmd::Status => {
            let s = light()?.status()?;
            if cli.json { println!("{}", serde_json::to_string_pretty(&s)?); return Ok(()); }
            let onoff = |b: Option<bool>| b.map(|b| if b { "ligada" } else { "desligada" }).unwrap_or("?");
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
                ModoArg::Cool => Mode::Cool, ModoArg::Warm => Mode::Warm,
                ModoArg::Balanced => Mode::Balanced, ModoArg::Ambient => Mode::Ambient,
            };
            light()?.set_mode(m)?
        }
        Cmd::Brightness { percent, raw } => {
            let mut l = light()?;
            let (mode, v) = match (percent, raw) {
                (_, Some(raw)) => { let m = l.mode()?; (m, l.set_brightness_raw(m, raw)?) }
                (Some(p), None) => l.set_brightness_percent(p)?,
                (None, None) => return Err("informe a porcentagem ou --raw".into()),
            };
            println!("{}: raw 0x{v:02x} ({}%)", mode.name(), light::raw_to_percent(mode, v));
        }
        Cmd::Image(ImageCmd::List) => {
            let ctrls = video()?.controls()?;
            if cli.json { println!("{}", serde_json::to_string_pretty(&ctrls)?); return Ok(()); }
            for c in ctrls {
                let faixa = match &c.kind {
                    ControlKind::Boolean => "on/off".to_string(),
                    ControlKind::Menu { items } => items.iter().map(|(i, n)| format!("{i}={n}")).collect::<Vec<_>>().join(", "),
                    ControlKind::Integer => format!("{}..{}", c.min, c.max),
                };
                println!("{:34} {:>6}  [{faixa}] padrão {}{}", c.key, c.value, c.default,
                    if c.inactive { "  (inativo: modo automático ligado)" } else { "" });
            }
        }
        Cmd::Image(ImageCmd::Get { controle }) => {
            let c = video()?.find(&controle)?;
            if cli.json { println!("{}", serde_json::to_string_pretty(&c)?); } else { println!("{}", c.value); }
        }
        Cmd::Image(ImageCmd::Set { controle, valor }) => {
            let v = video()?;
            let c = v.find(&controle)?;
            let value = parse_value(&c.kind, &valor)?;
            v.set(c.id, value)?;
            println!("{} = {}", c.key, v.get(c.id)?);
        }
        Cmd::Image(ImageCmd::Reset) => println!("{} controles no padrão", video()?.reset_defaults()?),
    }
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

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => { eprintln!("erro: {e}"); ExitCode::FAILURE }
    }
}
