
# 🚀 http-cockpit (hcp)

> Mission Control for your APIs.
> 
> A high-performance, TUI-based HTTP client engineered in Rust. Precise telemetry, zero-latency interface, and safety-first payload handling.

----------

## 📋 Mission Brief

Developing APIs often requires a trade-off: use `curl` (fast but complex to write) or Postman/Insomnia (easy but heavy, slow Electron apps).

**http-cockpit (hcp)** creates a new category. It provides the visual ergonomics of a GUI with the raw speed of the terminal. Built with a **"Reactor Pattern"** architecture, it ensures that your interface never freezes, even while streaming megabytes of data or negotiating TLS handshakes.

It’s not just a client; it’s a diagnostic tool that breaks down network latency into microsecond-level telemetry.

<img width="1184" height="623" alt="Captura de pantalla 2026-01-18 a la(s) 2 46 02 p m" src="https://github.com/user-attachments/assets/2fe346b1-1eae-42fb-ad76-4dd1d939d03b" />


## 🛰️ Key Capabilities

-   **⚡ Zero-Latency UI:** Decoupled Input Thread separates keyboard events from the Async Runtime. The interface responds instantly, always.
    
-   **🔬 Forensic Telemetry:** Don't just see "Time: 200ms". See the breakdown: **DNS Lookup** vs **TCP Handshake** vs **Server Processing** vs **Transfer**.
    
-   **🛡️ Active Safety:** Pre-flight checks validate your JSON syntax before the request leaves the terminal. No more accidental `400 Bad Request` due to a missing comma.
    
-   **📑 Multi-Tab Input:** Dedicated workspaces for **Body** (JSON) and **Headers** (Key-Value), navigable via shortcuts.
    
-   **🌊 Stream Processing:** Uses `reqwest::Body::wrap_stream` to handle large payloads without spiking RAM usage.
    
-   **⌨️ Vim-Style Navigation:** Keep your hands on the keyboard. Use `j`/`k` to scroll through massive responses.
    

## 🛠️ Installation

### Option 1: Cargo (Recommended)

If you have the Rust toolchain installed:

Bash

```
cargo install hcp

```

### Option 2: Build from Source

Bash

```
git clone https://github.com/lear94/hcp.git
cd hcp
cargo install --path .

```

## 🕹️ Flight Manual

Launch the application simply by typing:

Bash

```
hcp

```

### Flight Deck Controls

**Key**

**Action**

**Description**

**Navigation**

`Tab`

**Cycle Focus**

Rotate between URL, Method, Input, and Response panels.

`Shift+Tab`

**Back Focus**

Rotate focus in reverse.

**Input Management**

`1`

**Body Tab**

Switch editor to Request Body (JSON).

`2`

**Headers Tab**

Switch editor to Request Headers.

`Space` / `Enter`

**Toggle Method**

Cycle HTTP verbs (GET, POST, PUT, DELETE) when focused.

**Mission Control**

`Ctrl + s`

**Launch**

**Execute Request.** Validates JSON and fires the network engine.

`j` / `↓`

**Scroll Down**

Scroll the Response view down.

`k` / `↑`

**Scroll Up**

Scroll the Response view up.

`q`

**Quit**

Abort mission and exit.

## ⚙️ How it Works (Architecture)

`hcp` is built on a robust **Asynchronous Reactor** architecture to guarantee performance:

1.  **The Input Thread:** A dedicated OS thread captures `crossterm` events. It blocks efficiently when idle and sends signals via a non-blocking channel. This prevents the UI from "freezing" during heavy network I/O.
    
2.  **The Network Engine:** Powered by `Tokio` and `Reqwest`. It executes requests in the background, streaming bytes as they arrive and calculating precise monotonic timestamps for telemetry.
    
3.  **The State Machine:** The UI is a pure function of the application state, rendered efficiently by `Ratatui` on every tick of the event loop.
    

## 🧪 Example Workflow

1.  **Select Method:** Tab to Method, set to `POST`.
    
2.  **Set Target:** Tab to URL, enter `https://httpbin.org/post`.
    
3.  **Auth (Optional):** Press `2`, enter `Authorization: Bearer my-token`.
    
4.  **Payload:** Press `1`, enter `{ "system": "ready" }`.
    
5.  **Launch:** Press `Ctrl+S`.
    
6.  **Analyze:** Observe the breakdown of DNS vs Transfer time in the Telemetry panel.
    
## 📄 License

Distributed under the MIT License. See `LICENSE` for more information.
