# CyberCalls

Live call subtitles in the style of *Cyberpunk: Edgerunners*, popping up right on your desktop.

While you're on an AyuGram call, everything said by you and the other person is transcribed locally with
[Whisper](https://github.com/ggml-org/whisper.cpp) and appears as a stack of animated **CALL** boxes with
typewriter text: your lines in green on the right, theirs in red on the left. No cloud, nothing leaves your machine.

- Speakers are told apart by audio stream, not guessed: the call's playback is "them", the microphone is "you".
- English and Russian, detected per phrase, so mixing languages mid-call works.
- A phrase shows up roughly 1 s after it's finished (0.8 s of silence ends it, Whisper takes ~0.2–0.5 s);
  long sentences show partial text while they're still being spoken.
- The overlay is click-through, never takes focus, and only exists while a call is live.

## Requirements

- **Hyprland** (Wayland). The overlay uses `wlr-layer-shell`; call detection uses Hyprland's IPC socket.
- **PipeWire**
- **AyuGram** (other Telegram Desktop forks may work, see [Configuration](#configuration))
- **Nix with flakes** for the build environment. Without Nix you need Rust, `pkg-config`, `cmake`, `clang`/libclang,
  `glslc` (shaderc), and the PipeWire, Wayland, xkbcommon and Vulkan development packages.
- A Vulkan-capable GPU is recommended. Integrated Intel graphics is enough.

## Setup

```bash
cd CyberCalls
nix develop                # or `direnv allow` once, with programs.direnv.enable = true

# Whisper models aren't in git; get the default one (~180 MB)
mkdir -p models
curl -L -o models/ggml-small-q5_1.bin \
  https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small-q5_1.bin

cargo build --release      # first build compiles whisper.cpp + Vulkan shaders, takes a while
```

Always build with `--release`. Debug builds of Whisper are unusably slow.

## Usage

```bash
cargo run --release -- --demo   # ~30 s scripted fake call, to preview the look
cargo run --release             # the real thing: leave it running, then call someone
```

Startup takes a few seconds while Whisper loads and warms up. After that it idles until AyuGram opens a call window
**and** starts recording from the microphone. The other person's name is taken from the call window's title.

To see each phrase, the detected language, and how long transcription took:

```bash
RUST_LOG=cybercalls=debug cargo run --release
```

## Configuration

Optional. Create `~/.config/cybercalls/config.toml`; anything you leave out keeps its default.

```toml
my_name = "frantix"              # shown above your own bubbles
model = "/path/to/CyberCalls/models/ggml-small-q5_1.bin"   # default: models/ in the source tree
use_gpu = true                   # run Whisper on the GPU (Vulkan)
threads = 8                      # CPU threads for Whisper
languages = ["en", "ru"]         # each phrase is matched to one of these

app_class = "com.ayugram.desktop"  # Hyprland window class of the Telegram client
app_node = "AyuGram"               # substring of the client's PipeWire node.name

[overlay]
center_x = 0.5        # horizontal center of the bubble column, fraction of screen width
top = 0.10            # where the column starts, fraction of screen height
bottom = 0.92         # oldest lines are retired once the stack reaches this
column_width = 900.0  # px
box_size = 112.0      # side of the CALL box in px; text scales with it
linger_secs = 14.0    # how long a finished line stays before fading
chars_per_sec = 55.0  # typewriter speed
```

For another Telegram client, find its window class with `hyprctl clients` and its audio node name with `pw-dump`
during a call, then set `app_class` and `app_node`. Only AyuGram has been tested.

### Choosing a model

Measured on an i7-13620H with Intel UHD graphics (Vulkan), per spoken phrase:

| Model | Size | Languages | Time per phrase | Notes |
|---|---|---|---|---|
| `ggml-small-q5_1.bin` | 182 MB | multilingual | 0.15–0.55 s | **default**; good English, readable Russian with small slips |
| `ggml-medium-q5_0.bin` | 515 MB | multilingual | 0.6–2 s | noticeably better Russian, can lag on long phrases |
| `ggml-small.en-q5_1.bin` | 182 MB | English only | 0.1–0.5 s | set `languages = ["en"]` |
| `ggml-large-v3-turbo-q5_0.bin` | 548 MB | multilingual | 40 s+ | too slow on integrated graphics |

All of these are at `https://huggingface.co/ggerganov/whisper.cpp/resolve/main/<file>`.

## How it works

```
Hyprland IPC ── call window opens/closes, its title = peer name ─┐
                                                                 ├─► coordinator ─► Whisper ─► overlay
PipeWire ── tap the client's playback (peer) and its mic (you) ──┘     (VAD)        (en/ru)    (layer shell)
```

1. **Call detection** (`src/hypr.rs`, `src/audio.rs`): a call is live while AyuGram has a non-main window open
   *and* a microphone capture stream. Requiring both keeps voice-message recording from triggering it.
2. **Audio** (`src/audio.rs`): PipeWire capture streams are linked straight to AyuGram's playback stream and to the
   microphone it records from, converted to 16 kHz mono by PipeWire itself.
3. **Segmentation** (`src/vad.rs`): an energy-based voice detector with an adaptive noise floor cuts each feed into
   phrases, and emits snapshots every 1.5 s for live partial text.
4. **Transcription** (`src/stt.rs`): whisper.cpp via `whisper-rs`. Three things keep it fast and clean:
   - the encoder window is shrunk from Whisper's fixed 30 s to the phrase's length (about 20× faster);
   - the language is picked between the configured ones only, with a bias toward what that person spoke last;
   - repetition loops and Whisper's well-known phantom lines ("Thanks for watching", "Субтитры сделал…") are filtered.
   Finished phrases are always processed before partial ones.
5. **Overlay** (`src/overlay.rs`, `src/callbox.wgsl`): a fullscreen, click-through `wlr-layer-shell` surface drawn with
   wgpu. The CALL box (scanlines, grid, corner brackets, spinning two-arc ring, glitch/RGB-split pop-in) is a single
   shader; text is rendered with glyphon in the Tektur font. The ring spins faster while that person is talking.

The layer surface uses the namespace `cybercalls`, if you want to target it with Hyprland layer rules (for example,
to turn off Hyprland's own open animation).

## Benchmarking transcription

`examples/bench.rs` runs the segmenter and Whisper over a recording and prints each phrase with its timing:

```bash
pw-record --rate 16000 --channels 1 --format s16 test.wav    # Ctrl+C to stop
cargo run --release --example bench -- models/ggml-small-q5_1.bin test.wav        # GPU
cargo run --release --example bench -- models/ggml-small-q5_1.bin test.wav cpu    # CPU
```

Environment knobs: `LANGS=en,ru`, `FULL_CTX=1` (disable the shrunken encoder window), `CTX_PAD`, `CTX_MIN`.

## Troubleshooting

- **Nothing appears during a call.** Run with `RUST_LOG=cybercalls=debug`. If you see
  `client is recording but no call window is open`, the call window wasn't recognized; check its title and class with
  `hyprctl clients`.
- **Your mic shows the other person's words too.** Your mic is hearing your speakers. Use headphones, or PipeWire's
  echo-cancel module.
- **Transcription is very slow.** Make sure you built inside `nix develop`. The flake sets
  `NIX_ENFORCE_NO_NATIVE=0`; without it Nix strips `-march=native` and whisper.cpp loses AVX2/FMA (roughly 8× slower).
  If you changed that, force a rebuild with `cargo clean -p whisper-rs-sys --release`.
- **Russian comes out as English gibberish.** You're probably on an English-only model (`.en` in the file name).
  With `ru` in `languages`, the app exits at startup with an error saying so.

## Known limitations

- Only tested with AyuGram on Hyprland.
- Group voice chats are untested. If they work, everyone except you will show up under the chat's name.
- If a separate AyuGram chat window is open while you record a voice message, it can be mistaken for a call.
- No HiDPI scaling yet. Sizes are in physical pixels.

## Credits

- Speech recognition: [whisper.cpp](https://github.com/ggml-org/whisper.cpp) through [whisper-rs](https://codeberg.org/tazz4843/whisper-rs).
- Font: [Tektur](https://github.com/hyvyys/Tektur), SIL Open Font License (`assets/fonts/OFL.txt`).
- Visual style inspired by *Cyberpunk: Edgerunners*; no assets from the show are included.
