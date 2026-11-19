# AULA F75 protocol (2.4 GHz receiver and USB-C cable)

Recovered by static analysis of AULA's official Windows driver and verified on a
real F75 over the USB-C cable and the 2.4 GHz receiver. Values marked
**unverified** come from the decompiled code only.

## Source

| Item | Value |
|---|---|
| Installer | `AULA F75 driver.exe` from aulastar.com (F75 entry, aid=548) |
| SHA-256 | `f9938e1e365114101f5963ef7826fbb554ab8fcc4ea4ce792a96458c456ad886` |
| Packaging | Inno Setup 5.3.3, unpacked with `innoextract` (never executed) |
| Main binary | `app/OemDrv.exe`: PE32, MFC, not packed (`.text` entropy 6.59) |
| Tooling | Ghidra 12.1.4 headless, scripts in `tools/ghidra` |
| Device config | `app/Dev/kb/1/KB.ini`: `VID_Wireless=0x3554`, `PID_Wireless=0xfa09`, `MatrixLen=128`, `CRC=1` |

Bluetooth (`3554:fa08`, "AULA-F75 3.0 KB") exposes the same vendor collection
(usage page `0xFF02`, report `0x13`, 19 bytes in/out) as the receiver. On
hardware only one packet of each multi-packet reply arrives and config traffic
disrupts typing, and AULA's own software has no Bluetooth support, so
f75-studio treats Bluetooth as view-only.

The receiver is driven by class `CDev3632` (vtable `0x0060dd6c`). High-level
lighting code lives in `CDevG5KB`; when a receiver is present, every `CDevG5KB`
method forwards to the `CDev3632` object stored at `this+0x23c`
(e.g. `0x0048bbb0`, `0x0048be00`).

## Transport

* Linux: `hidraw` node for interface 1 of `3554:fa09` (descriptor starts with
  usage page `0xFF02`, report `0x13`, 19-byte input + output).
* Windows uses `WriteFile`/`ReadFile` of 20 bytes (`0x00486f40`, `0x00485af0`).
  Linux `hidraw` framing is identical (report ID first).
* The same interface also carries NKRO keyboard (`0x04`), consumer (`0x05`),
  system (`0x03`) and mouse (`0x07`) input reports; filter on byte 0 = `0x13`.

## Packet

```
byte  0     0x13 (report id)
byte  1     command
byte  2     total packets
byte  3     packet index (0-based)
byte  4     (sub << 4) | payload length (0..14)
bytes 5-18  payload
byte  19    checksum = sum(bytes 0..18) & 0xff        (0x00486d80)
```

### Write (`CDev3632::AccessData`, `0x004862f0`)

1. Split data into 14-byte chunks.
2. Before each packet wait `delay` ms (15 for settings, 10 for live data).
3. Unless the command has bit 7 set or is `0x06`, read replies until one has
   byte 1 == command (max 20 reads, 1 s each, `0x00485fa0`).
   * byte 0 == `0xff` → error; byte 2 bit 7 → CRC error (retry, max 5).
   * byte 3 must equal the packet index.

### Read (`0x004865d0`, `SendCMD_3632` `0x00487070`)

Request: `13 <cmd> 01 00 <sub<<4> 00… <cks>`. Replies: byte 1 & 0x7f = cmd,
byte 2 & 0x7f = total, byte 3 = index, byte 4 & 0x0f = length, data at byte 5.
Repeated indices are ignored; reading stops at `index + 1 == total`.

## Commands (CDev3632 vtable)

| Write | Read | vtable slot | Meaning |
|---|---|---|---|
| `0x01` | `0x41` | +0x50 / +0x54 | Key matrix, sub = layer |
| `0x02` | `0x42` | +0x68 / +0x6c | Per-key LED matrix, sub = colour group (vendor log text: "SetGame") |
| `0x03` | `0x43` | +0x58 / +0x5c | Macro data |
| `0x04` | `0x44` | +0x60 / +0x64 | LED settings block (128 bytes) |
| `0x06` | - | +0x80 | Reset device (no ack) |
| - | `0x07` | +0x2c | Online flag, 1 byte → `this+0x238`, logged as `m_bOnline` (not battery; KB.ini has `ShowPower=0`) |
| `0x09` | `0x49` | +0x70 / +0x74 | LED RGB table |
| `0x0a` | `0x4a` | +0x78 / +0x7c | On-board profile |
| `0x0b` | - | +0x4c | Screen parameters (not used on the plain F75) |
| `0x88` | - | +0x38 | Real-time frame, no ack |

## LED settings block (128 bytes, `GET 0x44` / `SET 0x04`)

The app always reads the block, checks `block[126] == 0x5A && block[127] == 0xA5`,
edits a few fields (`0x0048d810`) and writes it back.

| Offset | Meaning |
|---|---|
| 9 | 1 when the selected mode is the per-key "self-define" mode |
| 10 | Hardware effect id (table below) |
| 11 | Colour group shown by the self-define mode (base + group) **unverified** |
| `0x12` (18) | Side light mode: `0` Off, `1` Fill, `2` Breathing, `3` Static. Preserve the rest of the block when writing. |
| 0x38, 0x39 | Set to `0xff` |
| 0x3a + 2·i | Brightness for effect slot *i* (0-9) |
| 0x3b + 2·i | `(speed << 4) | colour` for slot *i* (speed 0-4) |

Colour nibble: 0 red, 1 green, 2 blue, 3 yellow, 4 magenta, 5 cyan, 6 white,
7 random (`RandomVal`, default 7).

### Effects (KB.ini `LedOpt*` + `Text/en/text.xml`)

`LedOpt` columns: hardware id, effect, speed, light, direction, random, colour.

| HW id | Name | HW id | Name |
|---|---|---|---|
| 1 | Fixed on | 10 | Retro snake |
| 2 | Respire | 11 | Neon stream |
| 3 | Rainbow | 12 | Reaction |
| 4 | Flash away | 13 | Sine wave |
| 5 | Raindrops | 14 | Retinue scanning |
| 6 | Rainbow wheel | 15 | Rotating windmill |
| 7 | Ripples shining | 16 | Colorful waterfall |
| 8 | Stars twinkle | 17 | Blossoming |
| 9 | Shadow disappear | 18 | Rotating storm |
| 21 | Self-define (per-key) | 0 | Off |

## Per-key colours (`0x02`, from `CDevG5KB::ApplySetting` `0x0048dbc0`)

* One buffer per colour group, `MatrixLen * 3` = 384 bytes sent.
* Layout: three 126-byte planes ordered by `GameRGBIndex` (`0,1,2` = R, G, B),
  then 6 bytes of padding.
* `plane[c][led] = colour channel c` where `led` is the 4th value of each
  `K<n>=` line in `KB.ini` (`led = column * 6 + row`, Esc = 0, F1 = 12, …).
* Colour source is a Windows `COLORREF` (`0x00BBGGRR`).

## Real-time frame (`0x88`, from `0x0047f250`)

* 126 LEDs × interleaved RGB (`RGBIndex` order) = 378 bytes, sub = 0.
* Sent without acknowledgements, 10 ms between packets in the vendor app
  (≈27 packets → ~3-4 frames/s over 2.4 GHz). **Unverified** whether the
  keyboard needs to be in a specific mode to display it.

Unsolicited input reports with byte 1 = `0x0a` and byte 5 = `0x02` carry the
online flag in byte 6 (`0x00485f10`).

## Wired (`258a:010c`, class `CDevG5KB`)

Interface 1, feature report `0x06` (descriptor: `85 06 … 96 07 02 b1 02` = 519 bytes + id).

```
byte 0     0x06 (report id; 0x09 on other models, chosen by device type 0x18)
byte 1     command (reads: command | 0x80)
byte 2     sub (layer / colour group)
byte 3     0
byte 4     total packets
byte 5     packet index
byte 6-7   payload size, little endian (≤ 512)
byte 8..   payload
```

* Write (`0x0048b4d0`): wait 20 ms, `HidD_SetFeature` (retry ×3, 70 ms apart).
* Read (e.g. GetLED `0x0048bc00`): `SetFeature` the header with `cmd | 0x80`, wait
  20 ms, `GetFeature` 520 bytes, payload at byte 8.

| Function | Wired cmd | 2.4G / BT cmd |
|---|---|---|
| Key matrix (sub = layer) | `0x03` | `0x01` |
| LED settings block | `0x04` | `0x04` |
| Macro buffer | `0x05` | `0x03` |
| Per-key LED matrix (sub = group) | `0x06` | `0x02` |
| Real-time frame | `0x08` (7 ms) | `0x88` |
| RGB table | `0x0a` | `0x09` |
| Screen params | `0x0b` | `0x0b` |
| GIF params | `0x0d` | - |
| Reset | `0x11` | `0x06` |

## Sleep

`block[0x18]` = KB.ini `SleepHW` value (2,3,4,6,8,10,20,30,40 for 60…1200 s);
the app writes `0` when sleep is disabled (`0x0048d810`). The vendor text says
sleep only applies in wireless mode. The user's keyboard reported `0x02` (60 s).

## Key matrix (`0x0048d2d0`, `0x0048c820`)

* 4 layers, each `MatrixLen × 4` = 512 bytes; the key at matrix index *n* lives at
  bytes `4n..4n+3`. The app takes *n* from the last field of each `K<n>=` line,
  the same number used as LED index.
* With KB.ini's flag the last entry of a layer is `00 00 5a a5`.
* Codes (little-endian u32 written as bytes):
  * keyboard: `[0x00, modifiers, 0x00, HID usage]` (e.g. Space `00 00 00 2c`)
  * consumer: `[0x02, 0x00, usage_hi, usage_lo]` (Play/Pause `02 00 00 cd`)
  * macro: `[0x03, mode, count, macro_index]`; mode 1 = play *count* times,
    2 = repeat while held, 4 = toggle
* f75-studio only writes after reading the layer and checking that Esc
  (matrix 0) reads back as `00 00 00 29`.

## Macro buffer (`0x0048d2d0`, `0x0048cf00`)

```
N × [addr u16, size u16]        addr counted from buffer start, size includes the name
per macro: name_len u8 (bytes), name (UTF-16LE, wcsnlen(name, 0x1e) → ≤ 30 chars), events…
event: [(type << 4) | up << 7 | delay[19:16], delay[15:8], delay[7:0], code]
```

Event type (`FUN_0048cf00`): the type variable starts at 0 and only the
modifier VKs (Shift/Ctrl/Alt/Win) set it to 1, so **0 = normal key** (HID
usage), 1 = modifier (`0xe0-0xe7`), 2 = mouse button (1,2,4,8,16), 3/4 = mouse
X/Y, 5 = wheel. Bit 7 marks key-up (`InitByMacro` 0x004028a0: editor flag
`& 1` = press). Delay in ms (20 bits) is the wait after the event.

Size written (`FUN_0048d2d0` → `CDev3632::ApplySetting`): the builder gets
`MacroBufferSize` (KB.ini has none → `0x1000`) as capacity and writes the
**used** size (table + blobs) back through the same pointer; `SetMacro` sends
only that many bytes, and only when at least one macro exists. 2.4G
(`0x004868a0`): 512-byte pages, `AccessData(cmd 0x03, sub = page)`, 100 ms
apart. Cable (`0x0048b960`): one `0x05` transfer. ApplySetting sleeps before
the macro write and writes the key matrix first.

Key matrix code for a macro key: `[0x03, mode, count, index]`, written by the
first FillMatrix pass; the second pass sets byte 3 to `Macro_Buffer_ID - 1`.

Earlier f75-studio builds tagged every key event as type 1 (modifier) and
padded the write to 4 KB, which is why macros didn't type; both are fixed and
covered by `macros::tests`.

## Compact real-time data (`0x0047f250`)

`realtime` with sub 2 takes groups `[R, G, B, count, led…]` (≤ 200 bytes); three
zero bytes end the stream. f75-studio currently streams full frames (sub 0).

## Not done yet

* Mouse events in macros, and macro modes other than "play once".
* Macro read-back over 2.4G (`0x43`) is used by `doctor` but not traced in OemDrv.
* `Psd=3,0,0,0,0,cd` in KB.ini: purpose not traced.
