//! Servidor MCP por stdio (JSON-RPC 2.0, uma mensagem por linha).
//! Cada chamada abre o dispositivo, age e fecha: a câmera pode ser desconectada entre chamadas.

use emeetctl_core::{light, ControlKind, Light, Mic, Mode, Target, Video};
use serde_json::{json, Value};
use std::io::{BufRead, Write};

const PROTOCOL_VERSION: &str = "2025-06-18";

fn tools() -> Value {
    json!([
        {
            "name": "get_status",
            "description": "Lê o estado da câmera EMEET S600L: dados do dispositivo (produto, serial, firmware, programas usando a câmera agora em in_use_by), da luz (fill light e anel RGB ligados, modo, cor, brilho) e do microfone (ganho, mudo, padrão).",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "set_light_power",
            "description": "Liga ou desliga a luz. target 'ambient' = anel RGB; 'fill' = luz de preenchimento inteira.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target": { "type": "string", "enum": ["ambient", "fill"] },
                    "on": { "type": "boolean" }
                },
                "required": ["target", "on"]
            }
        },
        {
            "name": "set_light_color",
            "description": "Define a cor do anel RGB (qualquer cor) e põe o anel em cor fixa.",
            "inputSchema": {
                "type": "object",
                "properties": { "color": { "type": "string", "description": "RRGGBB, ex.: ff0000" } },
                "required": ["color"]
            }
        },
        {
            "name": "set_light_mode",
            "description": "Modo da luz: cool, warm, balanced (brancos), ambient (cor fixa) ou rainbow (arco-íris).",
            "inputSchema": {
                "type": "object",
                "properties": { "mode": { "type": "string", "enum": ["cool", "warm", "balanced", "ambient", "rainbow"] } },
                "required": ["mode"]
            }
        },
        {
            "name": "set_light_brightness",
            "description": "Brilho da luz em % (0-100) da faixa do modo atual.",
            "inputSchema": {
                "type": "object",
                "properties": { "percent": { "type": "integer", "minimum": 0, "maximum": 100 } },
                "required": ["percent"]
            }
        },
        {
            "name": "list_image_controls",
            "description": "Lista os controles de imagem (UVC): chave, tipo, faixa, padrão, valor atual e se está inativo por causa de um modo automático.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "set_image_control",
            "description": "Altera um controle de imagem pela chave de list_image_controls (ex.: zoom_absolute, focus_automatic_continuous). Para ajustar um manual inativo, desligue antes o automático correspondente.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "control": { "type": "string" },
                    "value": { "type": "integer" }
                },
                "required": ["control", "value"]
            }
        },
        {
            "name": "list_microphones",
            "description": "Lista os microfones USB conectados (o da câmera S600L, Yeti Nano e outros): placa, nome, vid:pid e modelo.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "get_mic_status",
            "description": "Lê um microfone: ganho em dB e faixa, se está mudo (on=false), fone (se houver), se é a entrada padrão do sistema e quais programas estão gravando dele (in_use_by).",
            "inputSchema": { "type": "object", "properties": { "device": { "type": "string", "description": "Qual microfone (de list_microphones): placa, apelido s600l/nano, vid:pid ou trecho do nome. Sem ele: a entrada padrão do sistema." } } }
        },
        {
            "name": "set_mic_gain",
            "description": "Ganho do microfone em dB, dentro da faixa de get_mic_status (S600L 0-20, Yeti Nano 0-33). Devolve o estado depois da mudança.",
            "inputSchema": {
                "type": "object",
                "properties": { "db": { "type": "number" }, "device": { "type": "string", "description": "Qual microfone (de list_microphones): placa, apelido s600l/nano, vid:pid ou trecho do nome. Sem ele: a entrada padrão do sistema." } },
                "required": ["db"]
            }
        },
        {
            "name": "set_mic_mute",
            "description": "Silencia (muted=true) ou liga (muted=false) o microfone. O botão de mudo físico do Yeti Nano não passa por aqui.",
            "inputSchema": {
                "type": "object",
                "properties": { "muted": { "type": "boolean" }, "device": { "type": "string", "description": "Qual microfone (de list_microphones): placa, apelido s600l/nano, vid:pid ou trecho do nome. Sem ele: a entrada padrão do sistema." } },
                "required": ["muted"]
            }
        },
        {
            "name": "set_mic_default",
            "description": "Torna o microfone a entrada de áudio padrão do sistema (PipeWire).",
            "inputSchema": { "type": "object", "properties": { "device": { "type": "string", "description": "Qual microfone (de list_microphones): placa, apelido s600l/nano, vid:pid ou trecho do nome. Sem ele: a entrada padrão do sistema." } } }
        },
        {
            "name": "set_clean_mic",
            "description": "Microfone limpo: cria (enabled=true) uma entrada nova '<microfone> (limpo)' com redução de ruído, corte de grave e cancelamento de eco do WebRTC por cima do microfone, ou desliga (enabled=false). make_default=true já a torna a entrada padrão. Só um microfone limpo por vez; ligar em outro microfone troca.",
            "inputSchema": {
                "type": "object",
                "properties": { "enabled": { "type": "boolean" }, "make_default": { "type": "boolean" }, "device": { "type": "string", "description": "Qual microfone (de list_microphones): placa, apelido s600l/nano, vid:pid ou trecho do nome. Sem ele: a entrada padrão do sistema." } },
                "required": ["enabled"]
            }
        },
        {
            "name": "set_headphones",
            "description": "Fone ligado ao microfone (ex.: Yeti Nano): volume em dB e/ou mudo. Só para microfones com saída de fone.",
            "inputSchema": {
                "type": "object",
                "properties": { "db": { "type": "number" }, "muted": { "type": "boolean" }, "device": { "type": "string", "description": "Qual microfone (de list_microphones): placa, apelido s600l/nano, vid:pid ou trecho do nome. Sem ele: a entrada padrão do sistema." } }
            }
        },
        {
            "name": "reset_image_controls",
            "description": "Volta todos os controles de imagem ao padrão de fábrica.",
            "inputSchema": { "type": "object", "properties": {} }
        }
    ])
}

type R = Result<Value, String>;

fn light() -> Result<Light, String> {
    let p = emeetctl_core::find().map_err(|e| e.to_string())?;
    let h = p.hidraw.ok_or("canal HID da câmera não encontrado")?;
    Light::open(&h).map_err(|e| e.to_string())
}

fn video() -> Result<Video, String> {
    let p = emeetctl_core::find().map_err(|e| e.to_string())?;
    let v = p.video.ok_or("nó de vídeo da câmera não encontrado")?;
    Video::open(&v).map_err(|e| e.to_string())
}

fn mic() -> Result<Mic, String> {
    Mic::open().map_err(|e| e.to_string())
}

/// Microfone do argumento `device`; sem ele, a entrada padrão do sistema.
fn mic_arg(a: &Value) -> Result<Mic, String> {
    Mic::find(a.get("device").and_then(Value::as_str)).map_err(|e| e.to_string())
}

fn str_arg<'a>(a: &'a Value, k: &str) -> Result<&'a str, String> {
    a.get(k)
        .and_then(Value::as_str)
        .ok_or(format!("argumento '{k}' ausente"))
}

fn int_arg(a: &Value, k: &str) -> Result<i64, String> {
    a.get(k)
        .and_then(Value::as_i64)
        .ok_or(format!("argumento '{k}' ausente"))
}

fn call(name: &str, a: &Value) -> R {
    let e = |e: emeetctl_core::Error| e.to_string();
    match name {
        "get_status" => {
            let info = emeetctl_core::find().map_err(e)?.info();
            let light = match light().and_then(|mut l| l.status().map_err(e)) {
                Ok(s) => json!(s),
                Err(m) => json!({ "error": m }),
            };
            let mic = match mic().and_then(|m| m.status().map_err(e)) {
                Ok(s) => json!(s),
                Err(m) => json!({ "error": m }),
            };
            Ok(json!({ "device": info, "light": light, "microphone": mic }))
        }
        "set_light_power" => {
            let t = match str_arg(a, "target")? {
                "fill" => Target::Fill,
                "ambient" => Target::Ambient,
                o => return Err(format!("target inválido: {o}")),
            };
            let on = a
                .get("on")
                .and_then(Value::as_bool)
                .ok_or("argumento 'on' ausente")?;
            light()?.set_power(t, on).map_err(e)?;
            Ok(json!({ "ok": true }))
        }
        "set_light_color" => {
            let rgb = light::parse_hex(str_arg(a, "color")?).map_err(e)?;
            light()?.set_color(rgb).map_err(e)?;
            Ok(json!({ "ok": true, "color": light::hex(rgb) }))
        }
        "set_light_mode" => {
            let m = str_arg(a, "mode")?;
            let mode = Mode::parse(m).ok_or(format!("modo inválido: {m}"))?;
            light()?.set_mode(mode).map_err(e)?;
            Ok(json!({ "ok": true, "mode": mode }))
        }
        "set_light_brightness" => {
            let p = int_arg(a, "percent")?.clamp(0, 100) as u8;
            let (mode, raw) = light()?.set_brightness_percent(p).map_err(e)?;
            Ok(
                json!({ "ok": true, "mode": mode, "raw": raw, "percent": light::raw_to_percent(mode, raw) }),
            )
        }
        "list_image_controls" => Ok(json!(video()?.controls().map_err(e)?)),
        "set_image_control" => {
            let v = video()?;
            let c = v.find(str_arg(a, "control")?).map_err(e)?;
            let value = int_arg(a, "value")? as i32;
            if let ControlKind::Menu { items } = &c.kind {
                if !items.iter().any(|(i, _)| *i == value) {
                    return Err(format!("valor {value} fora do menu {items:?}"));
                }
            }
            v.set(c.id, value).map_err(e)?;
            Ok(json!({ "ok": true, "control": c.key, "value": v.get(c.id).map_err(e)? }))
        }
        "set_clean_mic" => {
            use emeetctl_core::mic::{self, clean};
            let on = a
                .get("enabled")
                .and_then(Value::as_bool)
                .ok_or("argumento 'enabled' ausente")?;
            if on {
                let d = mic::find(a.get("device").and_then(Value::as_str)).map_err(e)?;
                clean::enable(&d).map_err(e)?;
                if a.get("make_default").and_then(Value::as_bool) == Some(true) {
                    clean::make_default().map_err(e)?;
                }
            } else {
                clean::disable().map_err(e)?;
            }
            Ok(json!(clean::status()))
        }
        "list_microphones" => Ok(json!(emeetctl_core::mic::list())),
        "get_mic_status" => Ok(json!(mic_arg(a)?.status().map_err(e)?)),
        "set_headphones" => {
            let m = mic_arg(a)?;
            if let Some(db) = a.get("db").and_then(Value::as_f64) {
                m.set_headphones(db as f32).map_err(e)?;
            }
            if let Some(muted) = a.get("muted").and_then(Value::as_bool) {
                m.set_headphones_on(!muted).map_err(e)?;
            }
            Ok(json!(m.status().map_err(e)?))
        }
        "set_mic_gain" => {
            let db = a
                .get("db")
                .and_then(Value::as_f64)
                .ok_or("argumento 'db' ausente")?;
            let m = mic_arg(a)?;
            m.set_gain(db as f32).map_err(e)?;
            Ok(json!(m.status().map_err(e)?))
        }
        "set_mic_mute" => {
            let muted = a
                .get("muted")
                .and_then(Value::as_bool)
                .ok_or("argumento 'muted' ausente")?;
            let m = mic_arg(a)?;
            m.set_on(!muted).map_err(e)?;
            Ok(json!(m.status().map_err(e)?))
        }
        "set_mic_default" => {
            let m = mic_arg(a)?;
            m.make_default().map_err(e)?;
            Ok(json!(m.status().map_err(e)?))
        }
        "reset_image_controls" => {
            Ok(json!({ "ok": true, "reset": video()?.reset_defaults().map_err(e)? }))
        }
        _ => Err(format!("ferramenta desconhecida: {name}")),
    }
}

fn handle(msg: &Value) -> Option<Value> {
    let id = msg.get("id")?.clone(); // notificação: sem resposta
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => {
            let version = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or(PROTOCOL_VERSION);
            json!({
                "protocolVersion": version,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "emeetctl", "version": env!("CARGO_PKG_VERSION") },
                "instructions": "Controla a webcam EMEET SmartCam S600L: luz (anel RGB e fill light), imagem (foco, zoom, exposição, balanço de branco) e microfones USB, o da câmera e outros como o Yeti Nano (ganho, mudo, fone, entrada padrão). Use get_status antes de mudar algo."
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({ "tools": tools() }),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            match call(name, &args) {
                Ok(v) => {
                    json!({ "content": [{ "type": "text", "text": serde_json::to_string_pretty(&v).unwrap() }] })
                }
                Err(m) => json!({ "content": [{ "type": "text", "text": m }], "isError": true }),
            }
        }
        _ => {
            return Some(json!({ "jsonrpc": "2.0", "id": id,
                "error": { "code": -32601, "message": format!("método não suportado: {method}") } }))
        }
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn main() {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => handle(&msg),
            Err(e) => Some(json!({ "jsonrpc": "2.0", "id": null,
                "error": { "code": -32700, "message": e.to_string() } })),
        };
        if let Some(r) = reply {
            let _ = writeln!(out, "{r}");
            let _ = out.flush();
        }
    }
}
