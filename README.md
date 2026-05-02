<div align="center">

<img src="./screenshots/icon.svg" alt="overlooked icon" width="120" height="120" />

# overlooked

**A native, Rust-built desktop chat client for local Ollama and any OpenAI-compatible API.**
DeepSeek, Claude, Gemini, Kimi, GLM, Qwen, Yi, Mistral, xAI, Groq, OpenRouter, Together, Fireworks, Cerebras, Perplexity, NVIDIA NIM, DeepInfra — all in one window.

[![Built with Rust](https://img.shields.io/badge/built%20with-Rust-dea584?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Dioxus](https://img.shields.io/badge/UI-Dioxus%200.7-7c3aed)](https://dioxuslabs.com/)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/platform-linux%20%7C%20macOS%20%7C%20windows-lightgrey)]()

</div>

---

GPT-style desktop chats faded from the conversation. Rust has the toolchain, the safety story, and the runtime — and almost nobody is shipping a serious native chat client. This is that client.

## Highlights

- **Local first.** Talk to Ollama / LM Studio on `localhost`, zero accounts.
- **Cloud ready.** Drop in a Tavily, DeepSeek, OpenAI, Claude, Gemini, or any OpenAI-compatible base URL plus key.
- **Streaming responses** for both Ollama (NDJSON) and OpenAI-format (SSE).
- **Web search built-in** via Tavily — toggle the globe in the input pill, the assistant runs `web_search` as a function call.
- **Multi-user** with per-user chats, settings, themes, and avatars. Argon2 password hashing.
- **Light + dark themes** with a one-click toggle and a custom accent color picker.
- **21 provider presets** with auto-routed URLs (no need to remember `/openai/v1/chat/completions` vs `/v1beta/openai/chat/completions`).
- **Tiny single binary** (~11 MB). No Electron, no Node, no Python sidecar.
- **Hotkeys**: `Enter` send, `Shift+Enter` newline, `Ctrl+N` new chat, `Ctrl+B` toggle sidebar, `Ctrl+,` settings, `Esc` close.
- **3-dot chat menu** with rename, pin, delete; auto-flips upward near the bottom of the list.
- **Per-message avatars** for both you and the model.
- **Hard input limits** (200 K char message, 200-char chat title, 512-char API fields) enforced both at the UI and on save.
- **Auto-dismissing error toasts** — failures surface visibly, then clear themselves.

## Screenshots

<table>
<tr>
<td><img src="./screenshots/light-theme.png" alt="Light theme"/></td>
<td><img src="./screenshots/dark-theme.png" alt="Dark theme"/></td>
</tr>
<tr>
<td align="center"><sub>Light theme</sub></td>
<td align="center"><sub>Dark theme</sub></td>
</tr>
</table>

## Supported providers

| Provider | Auth | Notes |
| --- | --- | --- |
| Ollama (local) | none | Default `http://localhost:11434`. NDJSON streaming. |
| LM Studio (local) | none | OpenAI-compatible at `http://localhost:1234`. |
| OpenAI | bearer | gpt-4o, gpt-4o-mini, o1, o3-mini |
| Anthropic Claude | bearer | claude-opus-4-7, claude-sonnet-4-6, claude-haiku-4-5 (OpenAI-compat endpoint) |
| DeepSeek | bearer | deepseek-chat, deepseek-reasoner |
| Google Gemini | bearer | gemini-2.0-flash, gemini-1.5-pro (OpenAI-compat endpoint) |
| xAI Grok | bearer | grok-4, grok-2-latest |
| Mistral | bearer | mistral-large-latest, codestral-latest |
| Moonshot Kimi | bearer | kimi-k2-0905-preview, moonshot-v1-128k |
| Zhipu GLM | bearer | glm-4.5, glm-4-plus, glm-4-air |
| Alibaba Qwen | bearer | qwen-max, qwen-plus, qwen-turbo (DashScope) |
| 01.AI Yi | bearer | yi-large, yi-medium |
| Groq | bearer | llama-3.3-70b-versatile, mixtral-8x7b-32768 |
| Cerebras | bearer | llama3.1-70b, llama-3.3-70b |
| Perplexity | bearer | sonar, sonar-pro, sonar-reasoning |
| OpenRouter | bearer | Any model in the OpenRouter catalog |
| Together AI | bearer | meta-llama/Llama-3.3-70B-Instruct-Turbo and more |
| Fireworks AI | bearer | accounts/fireworks/models/llama-v3p3-70b-instruct |
| DeepInfra | bearer | meta-llama/Llama-3.3-70B-Instruct, deepseek-ai/DeepSeek-V3 |
| NVIDIA NIM | bearer | meta/llama-3.3-70b-instruct, nvidia/llama-3.1-nemotron-70b-instruct |
| Custom | optional | Any OpenAI-compatible base URL ending in `/v1`. |

The base URL is autofilled from the preset; per-provider chat/models paths are appended for you.

## Quick start

### Requirements
- Rust (stable)
- [Dioxus CLI](https://dioxuslabs.com/learn/0.7/CLI/installation/): `cargo install dioxus-cli`
- Linux dev libs: `sudo apt install libwayland-dev libgtk-3-dev libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libxdo-dev libssl-dev pkg-config`
- For local inference: [Ollama](https://ollama.com) running at `http://localhost:11434`

### Build and run
```bash
git clone https://github.com/KPCOFGS/overlooked.git
cd overlooked
dx build --release
./target/dx/overlooked/release/linux/app/overlooked
```

If you hit a black window on Linux + NVIDIA, set:
```bash
WEBKIT_DISABLE_DMABUF_RENDERER=1 WEBKIT_DISABLE_COMPOSITING_MODE=1 ./overlooked
```

### Configure a provider
1. Click the gear icon in the sidebar.
2. **Provider** → pick your provider preset. The base URL fills in automatically.
3. **API key** → paste your key (Ollama needs no key).
4. **Model** → pick from the dropdown (fetched live) or type a custom name.
5. Apply. Open a new chat (Ctrl+N) and start typing.

### Enable web search (Tavily)
1. Sign up at [tavily.com](https://tavily.com) (free tier: 1,000 searches/month).
2. Settings → **Tools → Tavily API key** → paste.
3. In the chat input pill, click the globe button to toggle web search on. The assistant decides when to call it.

## Hotkeys

| Key | Action |
| --- | --- |
| `Enter` | Send the current message |
| `Shift+Enter` | Insert a newline |
| `Ctrl+N` | New chat |
| `Ctrl+B` | Toggle sidebar |
| `Ctrl+,` | Open / close settings |
| `Esc` | Close the open modal |

## Stack

- [Dioxus](https://dioxuslabs.com/) for the UI (native desktop target via webview).
- `reqwest` (with `stream`) for HTTP + streaming bodies.
- `rusqlite` (bundled) for local persistence.
- `tokio` async runtime.
- `argon2` for password hashing.
- `serde` / `serde_json` for the wire formats.
- `uuid` for chat IDs.

## How it works

- **Chats** live in the `chats` table, scoped by `user_id`.
- **Messages** live in the `messages` table, joined by `chat_id`.
- **Settings** are per-user (`user_settings` table, one row per user).
- **Users** are stored in `users`. The default user is the unauthenticated **guest** (id = 1).
- **Streaming**: SSE events are parsed with a buffer that splits on `\n\n` (OpenAI) or `\n` (Ollama). Tokens are appended to the visible bubble as they arrive.
- **Tool calls**: streamed `tool_calls` deltas are accumulated by index. On `finish_reason: "tool_calls"`, the registered tool (currently `web_search` via Tavily) is dispatched, the result is appended as a `role:"tool"` message, and the chat completion is re-issued. Loops up to 3 rounds.
- **Interrupting** a stream stops the read loop; the partial visible content is preserved.
- **Cancelled requests** that haven't produced any tokens drop their placeholder cleanly.
- **Light/dark theme** is driven by CSS custom properties on a `.theme-light` / `.theme-dark` outer class. The accent color is a single inline `--accent` CSS variable, so theme + accent compose freely.

## Security and input handling

- Every text setting is sanitized on load AND on save (control chars stripped, length capped on character boundaries).
- `String::truncate` is never used directly — `safe_truncate(s, max_chars)` walks chars to avoid panicking on a UTF-8 boundary (relevant for emoji / CJK).
- Numeric ranges are clamped at three layers: input event, save, and right before sending.
- Hex color values are validated (`#rrggbb` only); invalid input falls back to the default.
- Password hashing uses Argon2 with a per-password salt; passwords must be 8–128 chars and contain at least one letter and one digit.
- Usernames are restricted to `[A-Za-z0-9_-]{3,32}` and `guest` is reserved.

## Roadmap

- [x] Multi-provider chat with streaming
- [x] Light + dark themes, custom accent
- [x] Per-user login (Argon2) with guest default
- [x] Web search via Tavily (function-calling loop)
- [x] Auto-dismissing error toasts
- [x] Per-message avatars
- [ ] Native file picker for avatar upload
- [ ] MCP servers (browse / install / spawn / tool dispatch via `rmcp`)
- [ ] Customizable hotkeys
- [ ] Markdown / code-block rendering in messages
- [ ] Message history search

## Contributing

Issues and PRs welcome. Bug reports especially welcome if they include the provider, model, and a reproducible message.

## License

MIT. See [LICENSE](./LICENSE).
