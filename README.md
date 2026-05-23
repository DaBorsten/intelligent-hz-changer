<div align="center">

<img src="public/intelligent-hz-changer.svg" alt="Logo" width="80" height="80" />

# Intelligent Hz Changer

**Automatically switch your monitor's refresh rate based on running processes.**

[![Platform](https://img.shields.io/badge/platform-Windows-blue?logo=windows)](https://github.com)
[![Tauri](https://img.shields.io/badge/Tauri-v2-24C8D8?logo=tauri)](https://tauri.app)
[![React](https://img.shields.io/badge/React-19-61DAFB?logo=react)](https://react.dev)
[![Rust](https://img.shields.io/badge/Rust-stable-CE422B?logo=rust)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/license-MIT-green)](LICENSE)

---

*Launch a game → monitor jumps to higher Hz. Quit the game → drops back to lower Hz. Fully automatic.*

</div>

---

## ✨ Features

| Feature | Description |
|---|---|
| 🎮 **Auto Hz Switching** | Detects process start/stop and instantly changes refresh rate |
| 🖥️ **Visual Monitor Canvas** | Interactive multi-monitor layout — click any screen to configure it |
| 👁️ **Process Watcher** | Define which processes trigger Game Mode |
| 📈 **Live Status & Hz Graph** | Real-time refresh rate history over the last hour |
| 📋 **Event Log** | Filterable log of all Hz changes and process events |
| 🧪 **Hz Test** | Preview your Game Hz setting for 5 seconds before saving |
| 🔍 **Monitor Identification** | Overlay showing monitor numbers to match physical displays |
| 🌙 **Dark / Light / System Theme** | Follows Windows theme or set manually |
| 📦 **System Tray** | Runs silently in the background; close to tray instead of quitting |
| 🚀 **Autostart** | Optional Windows startup via registry |
| 🔄 **Auto-Updates** | Built-in updater — stay current automatically |

---

## 📸 Screenshots

### Monitor Setup
![Monitor Settings](docs/screenshots/Screenshot%20Monitor%20Settings.png)

---

## 🚀 Getting Started

### Download

Head to the [Releases](../../releases) page and download the latest `.msi` or `.exe` installer for Windows.

### First-Time Setup

1. **Open the app** — it starts in the system tray.
2. Go to the **Monitor** tab and click on the screen you want to control.
3. Set your **Game Hz** (e.g. `144`) and **Standard Hz** (e.g. `60`).
4. Switch to the **Processes** tab and add the `.exe` names of your games (e.g. `cyberpunk2077.exe`).
5. Hit **Save** — the watcher is now active. ✅

> **Tip:** Use **Identify** to show monitor numbers as an overlay on each physical screen.

---

## 🛠️ Development

### Prerequisites

| Tool | Version |
|---|---|
| [Rust](https://rustup.rs) | stable |
| [Node.js](https://nodejs.org) | 18+ |
| [Bun](https://bun.sh) | latest |
| [Tauri CLI v2](https://tauri.app/start/prerequisites/) | v2 |

### Setup

```bash
# Clone the repo
git clone https://github.com/your-username/intelligent-hz-changer.git
cd intelligent-hz-changer

# Install JS dependencies
bun install

# Start development server (hot-reload)
bun run tauri dev
```

### Build

```bash
# Build release installer
bun run tauri build
```

Outputs:
- `src-tauri/target/release/bundle/msi/` — Windows MSI installer
- `src-tauri/target/release/bundle/nsis/` — NSIS exe installer

### Bump Version

```bash
bun run bump
```

---

## 🏗️ Architecture

```
intelligent-hz-changer/
├── src/                        # React frontend (TypeScript)
│   ├── components/
│   │   ├── MonitorConfig.tsx   # Visual monitor selector + Hz config
│   │   ├── ProcessList.tsx     # Watched process management
│   │   ├── StatusView.tsx      # Live Hz graph + event log
│   │   └── SettingsTab.tsx     # App settings (theme, autostart, tray)
│   ├── App.tsx                 # Root layout + tab navigation
│   ├── types.ts                # Shared TypeScript interfaces
│   └── ThemeContext.tsx        # Dark/light theme provider
│
└── src-tauri/                  # Rust backend
    └── src/
        ├── main.rs             # Tauri commands entry point
        ├── watcher.rs          # Hz change watcher loop
        ├── process_watcher.rs  # Process detection (Win32 API)
        ├── process_icon.rs     # Process icon extraction
        └── settings.rs         # App settings + autostart (registry)
```

### How It Works

```
Process starts (e.g. game.exe)
    │
    ▼
process_watcher detects it
    │
    ▼
Is it in watched_processes?
    │ Yes
    ▼
Set monitor to Game Hz  ──────► emit "hz-changed" event
    │
    ▼
Process stops
    │
    ▼
No watched processes running?
    │ Yes
    ▼
Set monitor to Standard Hz ───► emit "hz-changed" event
```

---

## ⚙️ Tech Stack

| Layer | Technology |
|---|---|
| Frontend | React 19, TypeScript, Tailwind CSS v4 |
| Build | Vite 7, Bun |
| Backend | Rust (stable), Tauri v2 |
| Windows API | Win32 (display management, registry, process enumeration) |
| Icons | lucide-react |
| Updater | tauri-plugin-updater |

---

## 📝 License

[MIT License](LICENSE)
