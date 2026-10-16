````markdown
# Missão: mapear o que falta da webcam EMEET S600L (rodada 2, só captura passiva)

## Contexto
Estou fazendo um app open source para Linux (emeetctl) para a minha webcam **EMEET SmartCam S600L**.
A rodada 1 já mapeou toda a LUZ (HID vendor, interface 4, Report ID 0x09, frames `09 02 02 OP LL LL LL LL dados`,
opcodes 00–07). Agora falta o resto do que o **EMEET Studio** faz. Seu trabalho: capturar o tráfego USB
enquanto o app executa cada função que falta, marcar o horário exato de cada ação e entregar um zip para análise.

Fatos (use para filtrar):
- USB VID:PID `328f:00ef`, firmware 1.0.19. Na rodada 1 a câmera estava no endereço USB 2 do `\\.\USBPcap1`
  (confira de novo, porque pode mudar).
- **Canal suspeito principal desta rodada: UVC Extension Unit** GUID `{46394292-0cd0-4ae3-8783-3133f9eaaa3b}`
  (10 controles). Ela aparece como control transfer de classe: SET_CUR = `bmRequestType 0x21 bRequest 0x01`,
  GET_CUR = `0xa1 0x81`, GET_LEN `0x85`, GET_INFO `0x86`, GET_MIN/MAX/RES/DEF `0x82/0x83/0x84/0x87`.
  O seletor fica no byte alto de `wValue` e o unit id no byte alto de `wIndex`.
- Canal secundário: o mesmo HID da luz (interrupt OUT ep 0x01 / IN ep 0x84), que pode ter opcodes novos (≥ 0x08).
- Controles UVC padrão (brilho, contraste, foco, zoom, exposição, balanço de branco, anti-flicker) já são conhecidos.
  Se uma ação gerar SET_CUR no Processing Unit ou no Camera Terminal, só registre qual controle foi; não é o foco.

## Regras duras
- **NUNCA envie nada para a câmera** por ferramenta nossa: nada de HID write, SET_CUR ou "teste". Só captura passiva.
- **NÃO clique em "Upgrade"/atualizar firmware**, nem em nada que pareça atualização.
- Capture **sem** `--inject-descriptors`, filtrando pelo endereço da câmera, com snaplen 4096
  (o vídeo isócrono infla o arquivo; o que importa é o controle).
- As ações no app sou EU que faço, ou você, controlando a interface **com cuidado**. Na rodada 1, cliques
  por coordenada caíram no lugar errado por causa de escala DPI e janela sobreposta. Antes de cada clique,
  confirme que a janela do EMEET Studio está visível e na frente. Depois de cada clique, confira na tela se
  a ação aconteceu e registre no CSV se não aconteceu.
- Toda função que você mexer, **volte ao estado original** no fim do bloco.

## Passos
1. Confirme o USBPcap e o endereço da câmera, e registre as versões (Windows, EMEET Studio, Wireshark/USBPcap).
2. **Inventário antes de capturar:** percorra TODAS as abas e telas do EMEET Studio e liste cada controle
   (nome exato, tipo, valores possíveis) em `C:\emeet-re2\INVENTARIO.md`, com prints de cada tela em
   `C:\emeet-re2\prints\`. Isso vale mais que a captura: é o mapa do que existe.
3. Uma captura por bloco (`tshark -i \\.\USBPcapN -w C:\emeet-re2\<bloco>.pcapng`), com o roteiro registrando
   `timestamp_iso_ms,acao` em `<bloco>-actions.csv`, ~4 s entre ações e um bloco `baseline` de 20 s sem ação.
   Blocos (pule o que não existir no app e registre no CSV que não existe):
   - `abrir`: fechar o EMEET Studio e abri-lo de novo com a câmera conectada (o app pode ler a Extension Unit
     inteira na abertura: GET_INFO/GET_LEN/GET_CUR de cada seletor; é o mapa dela).
   - `espelho`: espelhar horizontal liga/desliga; inverter vertical liga/desliga.
   - `presets`: cada preset de imagem ("Default", "Bright" ou o que existir); salvar um preset próprio.
   - `audio`: cancelamento de ruído / modos de áudio do microfone (se existir).
   - `desktop`: desktop mode / modo vertical / whiteboard (se existir) liga/desliga.
   - `efeitos`: na aba de efeitos/imagem, mexer em **cada** controle que não seja brilho, contraste, saturação,
     matiz, gama, nitidez, ganho, foco, zoom, exposição, balanço de branco ou anti-flicker
     (ex.: HDR, redução de ruído de imagem, auto-enquadramento, beleza) liga/desliga.
   - `luz-extra`: brilho mínimo, meio e máximo nos modos **Warm** e **Balanced** (falta a faixa deles);
     e qualquer opção de luz que não seja liga/desliga, modo, cor ou brilho.
   - `reset`: "restaurar padrão" / "reset" de imagem (se existir). **Não** resete nada de firmware.
   - `resolucao`: trocar resolução/fps no app (só para saber se o app manda algo além do formato UVC).
4. Pré-análise, sem descartar nada do pcap:
   - Liste **todos** os control transfers de classe para a interface de vídeo (unit id + seletor + dados),
     separando SET de GET, e todos os frames HID com opcode ≥ 0x08.
   - Para cada ação do CSV, liste o que saiu no intervalo (Enter anterior + 1,5 s, Enter atual + 1,5 s].
   - Do bloco `abrir`, monte a tabela da Extension Unit: seletor → GET_INFO, GET_LEN, GET_MIN/MAX/DEF e GET_CUR.
   - Marque as funções que **não geraram tráfego nenhum**: são feitas no PC, pelo software (espelho só na
     prévia, beleza, chroma key etc.).
5. `C:\emeet-re2\REPORT.md` com versões, inventário resumido, a tabela **ação → transfers em hex**, a tabela
   da Extension Unit, hipóteses e o que ficou ambíguo.
6. Zip com tudo (`.pcapng`, `*-actions.csv`, `INVENTARIO.md`, `prints\`, scripts, `REPORT.md`) em
   `C:\emeet-re2\emeet-s600l-round2.zip`. Me diga o caminho e o tamanho.

## Entrega
O zip é a entrega. Não publique nada em lugar nenhum.
````
