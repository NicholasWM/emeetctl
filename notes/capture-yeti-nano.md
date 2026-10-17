# Captura passiva do Blue Yeti Nano no Windows

Objetivo: mapear o canal HID do fabricante (report 2, 63 bytes) que o app oficial usa para padrão polar, mix de monitoramento e mudo, do mesmo jeito que o protocolo de luz do emeetctl foi mapeado (captura passiva, sem descompilar nada).

## Antes de começar

- App: Blue Sherpa (registre a versão) ou Logitech G HUB, se ele reconhecer o Nano. Anote qual.
- USBPcap: confirme o endereço USB do Nano e capture **só ele** (`--devices <addr>`), sem `--inject-descriptors`.
- **Não** clique em atualizar firmware, em nenhum momento.
- Inventário: print de todas as telas do app para o Nano em `prints\nano-*.png`, com cada controle (nome, tipo, faixa) no `INVENTARIO.md`.

## Blocos

Um `.pcapng` por bloco e um `<bloco>-actions.csv` com o horário de cada ação. Espere ~4 s entre ações.

| Bloco | Ações |
|---|---|
| `nano-abrir` | Fechar e abrir o app com o Nano conectado (a leitura inicial do estado mostra o mapa do report 2). |
| `nano-ganho` | Ganho mín → meio → máx → original. Ver se sai SET_CUR de áudio padrão (unit 5) ou HID report 2. |
| `nano-padrao` | Cardioide → omni → cardioide pelo app; depois pelo botão físico, se existir. |
| `nano-monitor` | Mix de monitoramento mín → máx → original. |
| `nano-mute` | Mudo pelo app; depois pelo botão físico (ver o que o mic manda ao PC). |
| `nano-taxa` | Trocar a taxa de amostragem no app. |
| `nano-voice` | Só se existir (G HUB): Blue VO!CE liga/desliga. Se não gerar tráfego USB, é processamento no PC. |
| `nano-knob` | Girar o knob do fone 3 cliques para cada lado (report 1, para confirmar). |

## Pré-análise esperada

Para cada ação: todo HID OUT / SET_FEATURE do report 2 em hex, e todo control transfer de classe (unidade, seletor, dados). Junte tudo num zip, como na captura da S600L.

## Contexto (lido no Linux, só leitura)

- Descritor HID: report 1 = mídia (Volume+/−, o knob); report 2 = fabricante, 63 bytes em entrada, saída e feature.
- Áudio padrão: captura mic → unit 5 → USB (mudo + volume 0–33 dB); reprodução USB → unit 6 → fone (mudo + volume −25 a +6 dB). 24 bits, 32/44,1/48 kHz.
- O único projeto público encontrado ([JackRozells/blueyeti-linux](https://github.com/JackRozells/blueyeti-linux)) descreve uma mensagem de report ID 0 que não bate com o descritor do Nano: não serve sem captura própria.
