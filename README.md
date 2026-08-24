# 🚀 http-cockpit (hcp)

> Mission Control for your APIs.
>
> A keyboard-driven TUI HTTP client in Rust. Real latency telemetry, an interface that never blocks, and payload handling that stays fast at megabyte scale.

---

## 📋 Mission Brief

Testing an API usually means choosing between `curl` (fast, but you write the request twice to get it right) and a desktop client (comfortable, but heavy).

**hcp** sits in between: the ergonomics of a GUI at the speed of a terminal. It is also a diagnostic tool — every request is broken down into the phases that actually cause latency, so "the API is slow" becomes "DNS is slow" or "the server is slow" in one glance.

<img width="1184" height="623" alt="hcp screenshot" src="https://github.com/user-attachments/assets/2fe346b1-1eae-42fb-ad76-4dd1d939d03b" />

## 🛰️ What it does

- **🔬 Real phase telemetry.** DNS, TCP+TLS handshake, server processing and body transfer are measured separately — through a custom resolver and a connector layer, not estimated. A phase that did not happen says so: a pooled connection reports `connection reused`, an IP literal reports `no lookup`.
- **⚡ An interface that never blocks.** Terminal input is read on its own OS thread and the network runs on Tokio, so the UI stays responsive during a slow TLS handshake or a 30 MB download. `Esc` aborts a request in flight.
- **📜 Megabyte-scale response viewing.** The response is laid out once per viewport size and only the visible rows are rendered, so scrolling a 30 MB payload costs the same as scrolling an empty one.
- **🛡️ Safe by default.** TLS certificates are verified unless you pass `--insecure` (and when you do, the UI says so in red). JSON request bodies are validated before the request leaves the terminal, with the offending character marked.
- **🎨 Response inspection.** JSON syntax highlighting and pretty-printing, response headers, raw view, hex dump for binary payloads, in-buffer search, wrapping toggle, clipboard copy over OSC 52, and byte-exact save to disk.
- **📚 It remembers.** Every sent request lands in a persistent history; name and save the ones you keep coming back to.
- **⌨️ Vim-friendly, mouse-optional.** `j`/`k`/`g`/`G`/`/`/`n` work as you would expect; the wheel scrolls and clicks move focus if you prefer.

## 🛠️ Installation

```bash
cargo install hcp
```

Or from source:

```bash
git clone https://github.com/lear94/hcp.git
cd hcp
cargo install --path .
```

Requires Rust 1.75 or newer. TLS is provided by `rustls`, so there is no OpenSSL dependency.

## 🕹️ Flight Manual

```bash
hcp                                        # open the cockpit
hcp https://api.example.com/v1/status      # preload a URL
hcp -X POST -d @body.json -s api.dev/v1    # fill it in and fire immediately
```

### Command line

| Flag | Meaning |
| --- | --- |
| `-X, --method <METHOD>` | `GET`, `POST`, `PUT`, `PATCH`, `DELETE`, `HEAD`, `OPTIONS` |
| `-H, --header <'K: V'>` | Add a header. Repeatable. |
| `-d, --data <BODY>` | Request body. `@file` reads a file, `@-` reads stdin. |
| `-t, --timeout <SECS>` | Total request timeout (default 30) |
| `--connect-timeout <S>` | Connection timeout (default 10) |
| `--max-body <MB>` | Response bytes to keep (default 32) |
| `-k, --insecure` | Skip TLS certificate verification |
| `--no-redirects` | Do not follow 3xx redirects |
| `--no-mouse` | Disable mouse capture, freeing terminal text selection |
| `--no-wrap` / `--no-pretty` | Start with wrapping / JSON formatting off |
| `-s, --send` | Fire the request as soon as the cockpit opens |
| `--print-config-path` | Where settings, history and saved requests live |

A URL without a scheme is completed for you: `api.dev/v1` becomes `https://api.dev/v1`, and `localhost:8080` becomes `http://localhost:8080`.

### Keys

**Mission**

| Key | Action |
| --- | --- |
| `Ctrl+S` · `F5` | Send the request |
| `Esc` | Abort the request in flight, or close an overlay |
| `Ctrl+Q` | Quit (`q` also quits when no text field has focus) |

**Navigation**

| Key | Action |
| --- | --- |
| `Tab` / `Shift+Tab` | Move focus forward / backward |
| `1` / `2` | Request body / headers (`Alt+1`, `Alt+2` while typing) |
| `←` / `→` · `Space` | Change HTTP method (method pane) |

**Response**

| Key | Action |
| --- | --- |
| `j` `k` `↑` `↓` | Scroll one line |
| `PgUp` `PgDn` · `Ctrl+U` `Ctrl+D` | Scroll one screen / half a screen |
| `g` / `G` | Jump to top / bottom |
| `←` / `→` · `t` | Cycle body / headers / raw |
| `h` / `l` | Scroll sideways (when wrapping is off) |
| `/` · `n` · `N` | Search · next hit · previous hit |
| `w` | Toggle line wrapping |
| `y` · `Ctrl+Y` · `F6` | Copy the current view to the clipboard |
| `s` · `F7` | Save the payload to a file, byte for byte |
| `Ctrl+L` | Clear the response |

**Requests**

| Key | Action |
| --- | --- |
| `Ctrl+R` · `F4` | History of sent requests |
| `F2` | Save the current request under a name |
| `F3` | Open a saved request |
| `?` · `F1` | Full key reference |

While a text field has focus the editor keeps its own keys — `Ctrl+W` deletes a word, `Ctrl+U` undoes, `Ctrl+R` redoes, `Ctrl+Y` pastes. The `F2`–`F7` keys reach every overlay from anywhere.

## ⚙️ How it works

1. **Input thread.** A dedicated OS thread blocks on `crossterm::event::read` and forwards events over a channel. Terminal input can never be delayed by network work, and the runtime is never blocked by a terminal read.
2. **Instrumented client.** A custom `Resolve` implementation times name resolution and a Tower connector layer times the TCP+TLS handshake. Subtracting the handshake from time-to-first-byte gives the server's own share. Timings are tagged with a request generation, so a reply from a request you cancelled can never be attributed to the current one.
3. **Viewport-bounded rendering.** The response buffer is indexed into display rows once per (width, wrap mode) pair; each frame renders only the rows on screen. Search hits are resolved to byte ranges at search time so highlighting never re-scans the document.
4. **Redraw on change.** The event loop only repaints when something actually changed, plus a light tick while a request is in flight, so an idle cockpit costs nothing.

## 💾 Where state lives

`hcp --print-config-path` prints the directory (`~/.local/share/hcp` on Linux, `~/Library/Application Support/hcp` on macOS, `%APPDATA%\hcp` on Windows). Set `HCP_DATA_DIR` to move it.

| File | Contents |
| --- | --- |
| `config.json` | Timeouts, redirect policy, wrap and formatting preferences |
| `history.json` | The last 200 requests you sent |
| `collection.json` | Requests you saved by name |

Writes are atomic, and a corrupt file is ignored rather than fatal.

## 🧪 Example workflow

1. `hcp api.example.com/v1/orders`
2. `Tab` to the method, `Space` until `POST`.
3. `2`, then type `Authorization: Bearer …`.
4. `1`, then type the JSON body — it is validated before launch.
5. `Ctrl+S`. Watch the waterfall to see whether the time went to DNS, the handshake, or the server.
6. `F2` to save the request for next time.

## 🔬 Development

```bash
cargo test        # unit tests plus an end-to-end suite against a local server
cargo clippy --all-targets
```

## 📄 License

MIT. See `LICENSE`.
