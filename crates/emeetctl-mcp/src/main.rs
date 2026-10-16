//! Servidor MCP por stdio (JSON-RPC 2.0, uma mensagem por linha).
//! Cada chamada abre o dispositivo, age e fecha: a câmera pode ser desconectada entre chamadas.

use emeetctl_core::{light, ControlKind, Light, Mode, Target, Video};
use serde_json::{json, Value};
use std::io::{BufRead, Write};

const PROTOCOL_VERSION: &str = "2025-06-18";

fn tools() -> Value {
    json!([
        {
            "name": "get_status",
            "description": "Lê o estado da câmera EMEET S600L: dados do dispositivo (produto, serial, firmware) e da luz (fill light e anel RGB ligados, modo, cor, brilho).",
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
            Ok(json!({ "device": info, "light": light }))
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
                "instructions": "Controla a webcam EMEET SmartCam S600L: luz (anel RGB e fill light) e imagem (foco, zoom, exposição, balanço de branco). Use get_status antes de mudar algo."
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
