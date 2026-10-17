//! "Microfone limpo": uma entrada nova no PipeWire com o processamento de voz
//! do WebRTC (o mesmo do Chrome/Meet) por cima de um microfone: redução de
//! ruído, corte de grave e cancelamento do eco do que toca nos alto-falantes.
//!
//! Roda como um cliente do PipeWire à parte (`pipewire -c <conf>`) num serviço
//! de usuário do systemd. Ligar e desligar não reinicia o PipeWire, e o
//! microfone original continua lá, intacto.

use super::{MicDevice, PipeWire};
use crate::{Error, Result};
use serde::Serialize;
use std::path::PathBuf;
use std::process::Command;

/// `node.name` da entrada limpa.
pub const NODE: &str = "emeetctl-clean";
pub const UNIT: &str = "emeetctl-clean-mic.service";

#[derive(Debug, Clone, Serialize)]
pub struct CleanStatus {
    /// O serviço está rodando.
    pub active: bool,
    /// Liga sozinho com a sessão.
    pub enabled: bool,
    /// `node.name` do microfone de origem, pela configuração gravada.
    pub source: Option<String>,
    /// A entrada limpa é a entrada padrão do sistema.
    pub default: bool,
}

fn config_dir() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .ok_or_else(|| Error::Invalid("HOME não definido".into()))?;
    Ok(base)
}

fn conf_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("emeetctl/clean-mic.conf"))
}

fn unit_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("systemd/user").join(UNIT))
}

fn systemctl(args: &[&str]) -> Result<String> {
    let out = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .map_err(|e| Error::Invalid(format!("systemctl: {e}")))?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status.success() || args[0].starts_with("is-") {
        Ok(text)
    } else {
        Err(Error::Invalid(format!(
            "systemctl {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

/// Configuração do cliente PipeWire: só o módulo de eco/ruído, ligado ao
/// microfone `source` (node.name) e publicado como `NODE`.
pub fn config(source: &str, description: &str) -> String {
    let q = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        r#"# Gerado pelo emeetctl ("microfone limpo"). Não edite: é reescrito ao ligar.
context.properties = {{ log.level = 0 }}
context.spa-libs = {{
    audio.convert.* = audioconvert/libspa-audioconvert
    support.*       = support/libspa-support
}}
context.modules = [
    {{ name = libpipewire-module-rt args = {{}} flags = [ ifexists nofail ] }}
    {{ name = libpipewire-module-protocol-native }}
    {{ name = libpipewire-module-client-node }}
    {{ name = libpipewire-module-adapter }}
    {{ name = libpipewire-module-echo-cancel
        args = {{
            library.name = aec/libspa-aec-webrtc
            # referência do eco = o que toca na saída padrão (sem criar outra saída)
            monitor.mode = true
            audio.channels = 1
            audio.position = [ MONO ]
            aec.args = {{
                webrtc.noise_suppression = true
                webrtc.high_pass_filter = true
                webrtc.gain_control = false
                webrtc.extended_filter = true
                webrtc.delay_agnostic = true
                webrtc.voice_detection = true
            }}
            capture.props = {{
                node.name = "{NODE}.capture"
                target.object = "{source}"
                node.passive = true
                # sem o microfone, não cai em outra entrada (nem na própria limpa)
                node.dont-reconnect = true
            }}
            source.props = {{
                node.name = "{NODE}"
                node.description = "{description}"
                media.class = Audio/Source
            }}
            sink.props = {{
                node.name = "{NODE}.reference"
            }}
        }}
    }}
]
"#,
        source = q(source),
        description = q(description),
    )
}

fn unit(conf: &std::path::Path) -> String {
    format!(
        "# Gerado pelo emeetctl (\"microfone limpo\").\n\
         [Unit]\n\
         Description=emeetctl: microfone limpo (redução de ruído e eco)\n\
         After=pipewire.service wireplumber.service\n\
         BindsTo=pipewire.service\n\
         \n\
         [Service]\n\
         ExecStart=/usr/bin/pipewire -c {}\n\
         Restart=on-failure\n\
         RestartSec=2\n\
         \n\
         [Install]\n\
         WantedBy=pipewire.service\n",
        conf.display()
    )
}

/// Liga o microfone limpo em cima de `mic` (troca de microfone se já estava
/// ligado em outro) e deixa ligado nas próximas sessões.
pub fn enable(mic: &MicDevice) -> Result<()> {
    let (_, source) = PipeWire::read()?
        .source_for_card(mic.card)
        .ok_or(Error::NotFound("entrada do microfone no PipeWire"))?;
    let conf = conf_path()?;
    let unit_file = unit_path()?;
    for p in [&conf, &unit_file] {
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir)?;
        }
    }
    std::fs::write(&conf, config(&source, &format!("{} (limpo)", mic.short())))?;
    std::fs::write(&unit_file, unit(&conf))?;
    systemctl(&["daemon-reload"])?;
    systemctl(&["enable", UNIT])?;
    systemctl(&["restart", UNIT])?;
    Ok(())
}

/// Desliga e não liga mais com a sessão. Se a entrada limpa era a padrão,
/// o PipeWire volta sozinho para outra entrada.
pub fn disable() -> Result<()> {
    systemctl(&["disable", "--now", UNIT])?;
    Ok(())
}

pub fn status() -> CleanStatus {
    let enabled = systemctl(&["is-enabled", UNIT]).unwrap_or_default();
    let source = conf_path()
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|c| {
            let line = c.lines().find(|l| l.trim().starts_with("target.object"))?;
            Some(line.split('"').nth(1)?.to_string())
        });
    let pw = PipeWire::read().ok();
    CleanStatus {
        active: systemctl(&["is-active", UNIT]).is_ok_and(|s| s == "active"),
        enabled: enabled == "enabled",
        source,
        default: pw
            .as_ref()
            .is_some_and(|p| p.default_source().is_some() && p.default_source() == p.node_id(NODE)),
    }
}

/// Torna a entrada limpa a entrada padrão do sistema.
pub fn make_default() -> Result<()> {
    // O nó aparece alguns instantes depois de o serviço subir.
    for _ in 0..20 {
        if let Some(id) = PipeWire::read()?.node_id(NODE) {
            return super::set_default(id);
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
    }
    Err(Error::NotFound("entrada limpa no PipeWire (está ligada?)"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_targets_the_mic_and_escapes_quotes() {
        let c = config("alsa_input.nano", "Yeti \"Nano\" (limpo)");
        assert!(c.contains(r#"target.object = "alsa_input.nano""#));
        assert!(c.contains(r#"node.description = "Yeti \"Nano\" (limpo)""#));
        assert!(c.contains("webrtc.noise_suppression = true"));
        assert!(c.contains("monitor.mode = true"));
    }
}
