# nuphus-mcp Tools Reference

This document describes every tool exposed by the current `nuphus-mcp` build.
All tools are defined by the MCP Server's `tools/list` response; this document
is the authoritative human-readable reference.

- **Total tools: 37** — Desktop: 15 · Browser: 22
- **Protocol**: JSON-RPC 2.0 over stdio (newline-delimited JSON)
- **Protocol version**: `2024-11-05`
- **Supported methods**: `initialize`, `notifications/initialized`, `ping`, `tools/list`, `tools/call`

---

## Table of Contents

- [Safety Annotations](#safety-annotations)
- [Calling a Tool](#calling-a-tool)
- [Vision & Local Models](#vision--local-models)
- [Desktop Tools (15)](#desktop-tools-15)
- [Browser Tools (22)](#browser-tools-22)
- [End-to-End Example](#end-to-end-example)

---

## Safety Annotations

Every tool carries an `annotations` field in `tools/list` (MCP spec).

- **`destructiveHint: true`** (26 tools) — write operations that change system or
  page state. Clients SHOULD surface a confirmation UI before invoking these.
- **`readOnlyHint: true`** (11 tools) — read-only operations, safe to auto-run.

Read-only tools (11):

| Tool |
|------|
| `desktop_screen_size` |
| `desktop_windows_list` |
| `desktop_window_info` |
| `desktop_vision` |
| `desktop_perceive` |
| `browser_snapshot` |
| `browser_extract` |
| `browser_cookies_get` |
| `browser_list_tabs` |
| `browser_list_downloads` |
| `browser_wait_for` |

All other tools (26) are marked `destructiveHint`. Note: `desktop_mouse` is
conservatively annotated destructive at the schema level because its `action`
may be `click`/`scroll`/etc. At runtime the confirmation check treats only
`action: "position"` as read-only. `desktop_vision` / `desktop_perceive` are
read-only (they read the screen); the first `desktop_perceive` call may
download OCR models as a side effect (see [Vision & Local Models](#vision--local-models)).

### Strict confirm mode

Pass `--confirm-write` on the command line or set
`NUPHUS_MCP_CONFIRM_WRITE=1` in the environment to enable **strict confirm
mode**. In this mode every write tool requires an explicit `"confirm": true`
argument; otherwise the call is rejected with `isError: true` and no side
effect occurs. Read-only tools are never affected.

```json
{
  "jsonrpc": "2.0",
  "id": 10,
  "method": "tools/call",
  "params": {
    "name": "desktop_input",
    "arguments": { "mode": "type", "hwnd": 123456, "text": "hello", "confirm": true }
  }
}
```

### Path validation

Screenshot save paths are validated to reject path traversal (`..`) and system
protected directories. `browser_upload` requires the file to actually exist;
`browser_drag_files` accepts only existing files or directories and canonicalizes
their paths before passing them to Chrome.

---

## Calling a Tool

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "tools/call",
  "params": {
    "name": "<tool_name>",
    "arguments": { "<param>": <value> }
  }
}
```

The response is a standard MCP `CallToolResult`. Failures that are semantically
expected (bad arguments, rejected paths, missing window, browser not
launchable) return `isError: true` with a human-readable message in
`content[0].text`. Protocol-level errors (unknown method, not initialized)
return a JSON-RPC `error`.

---

## Vision & Local Models

### `desktop_vision` — BYOK (bring your own key)

`desktop_vision` sends a screenshot to **your own** vision model. It supports
two protocols:
- **OpenAI-compatible** Chat Completions (default) — `image_url` content; works
  with OpenAI, MiniMax, Qwen, Ollama, vLLM, …
- **Anthropic native** Messages API — set `NUPHUS_MCP_VISION_BASE_URL` to
  `https://api.anthropic.com/v1` and the protocol is auto-detected from the host
  (or force it with `NUPHUS_MCP_VISION_PROVIDER=anthropic`).

Nothing is required unless you call it — and when it is not configured the tool
returns `isError: true` with a clear message naming the missing variable.

| Environment variable | Required | Default | Description |
|----------------------|----------|---------|-------------|
| `NUPHUS_MCP_VISION_API_KEY` | **yes** | — | API key for your vision model |
| `NUPHUS_MCP_VISION_BASE_URL` | no | `https://api.openai.com/v1` | Base URL (`https://api.anthropic.com/v1` for Claude) |
| `NUPHUS_MCP_VISION_MODEL` | **yes** | — | Model id, e.g. `gpt-4o-mini`, `qwen-vl-max`, `claude-sonnet-4-5` |
| `NUPHUS_MCP_VISION_PROVIDER` | no | `auto` | `auto` \| `openai` \| `anthropic`; `auto` infers from the base URL host |
| `NUPHUS_MCP_VISION_MAX_TOKENS` | no | `1024` | Max output tokens (Zhipu GLM-4V-Flash caps at 1024; raise for text-heavy screenshots) |

```sh
# OpenAI-compatible provider (default)
set NUPHUS_MCP_VISION_API_KEY=sk-...
set NUPHUS_MCP_VISION_MODEL=qwen-vl-max
# optional: set NUPHUS_MCP_VISION_BASE_URL=https://your-gateway/v1

# Anthropic / Claude — provider is auto-detected from the base URL
set NUPHUS_MCP_VISION_API_KEY=sk-ant-...
set NUPHUS_MCP_VISION_BASE_URL=https://api.anthropic.com/v1
set NUPHUS_MCP_VISION_MODEL=claude-sonnet-4-5
```

### `desktop_perceive` — local OCR + YOLO models

`desktop_perceive` runs PaddleOCR locally with ONNX Runtime. The first call
downloads the OCR models automatically into `%APPDATA%\Nuphus\models` (or
`NUPHUS_MODELS_DIR` if set). Download sources: `hf-mirror.com/SWHL/RapidOCR`
(det/rec ONNX) and `gitee.com/paddlepaddle/PaddleOCR` (char dictionary). If a
download fails the tool returns a clear error with manual download
instructions.

- **YOLO icon detection** (`icon_detect.onnx`) is auto-downloaded alongside the
  OCR models (source: `onnx-community/OmniParser-icon_detect_640x640`, hf-mirror
  first). It is *optional* at runtime: if its download fails, `desktop_perceive`
  still returns OCR elements and reports `yolo_available: false`. Set
  `NUPHUS_MCP_YOLO_MODEL_URL` to a direct `.onnx` URL to override the source
  (e.g. the full ~80 MB OmniParser export or a private mirror).
- `NUPHUS_MCP_NO_MODEL_DOWNLOAD=1` skips the automatic download (fast-fail on
  restricted networks / CI).
- Requires `onnxruntime.dll` on the library search path (bundled next to the
  Nuphus app; copy it next to `nuphus-mcp.exe` for standalone use).

### Recommended flow: `desktop_vision` + `desktop_perceive` together

These two tools are designed to be used **as a pair**:

1. `desktop_vision` — understand the screen (layout, text, icons) with your own
   vision LLM. It gives semantic understanding but **imprecise coordinates**.
2. `desktop_perceive` — locate exact UI element coordinates (local OCR + YOLO).
   It returns precise `center` coordinates but **no semantic understanding**.
3. Click using the `center` coordinate from `desktop_perceive` — never
   coordinates guessed from `desktop_vision`.

This mirrors the battle-tested flow in the Nuphus desktop app: *vision for
semantics, perceive for precision*.

---

## Desktop Tools (15)

Desktop tools control the local machine: screen, windows, mouse, keyboard and
clipboard. On Windows they are implemented on Win32 via the `desktop-api`
crate (xcap capture + SendInput); on macOS/Linux, mouse/keyboard fall back to
`enigo` and clipboard to `arboard`. macOS desktop input requires Accessibility
permission for the host application.

### desktop_screen_size

Get the screen resolution (width × height).

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| *(none)* | | | | No arguments |

**Example**
```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"desktop_screen_size","arguments":{}}}
```
**Returns** `{"width":1920,"height":1080}`

---

### desktop_screenshot

Fullscreen screenshot, or a region screenshot, saved as PNG (lossless). If
`path` is omitted the PNG data is returned inline as base64.

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `path` | string | no | - | Save path (auto-appends `.png`); omit to return base64 |
| `region` | object | no | - | Crop region `{x, y, width, height}`; omit for fullscreen |

**Example — inline base64**
```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"desktop_screenshot","arguments":{}}}
```
**Returns** `{"format":"png","data":"<base64>","width":1920,"height":1080}`

**Example — save to file**
```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"desktop_screenshot","arguments":{"path":"C:/Users/me/Desktop/shot.png"}}}
```
**Returns** `{"path":"C:/Users/me/Desktop/shot.png","format":"png","size":123456,"width":1920,"height":1080}`

---

### desktop_windows_list

List all visible OS windows (hwnd / title / position).

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| *(none)* | | | | No arguments |

**Example**
```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"desktop_windows_list","arguments":{}}}
```
**Returns** a JSON array of visible windows with `hwnd`, `title`, and position fields.

---

### desktop_window_activate

Bring a window to the foreground by `hwnd`. Window operations (screenshot,
click, input) MUST be preceded by activating the target window, otherwise they
may target the wrong window or fail.

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `hwnd` | integer | **yes** | - | Window handle from `desktop_windows_list` |

**Example**
```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"desktop_window_activate","arguments":{"hwnd":123456}}}
```

---

### desktop_window_screenshot

Capture a specific window as PNG, located by `hwnd` or `title` (at least one
must be provided). If `path` is omitted the PNG is returned as base64.

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `title` | string | no* | - | Window title substring to find |
| `hwnd` | integer | no* | - | Window handle from `desktop_windows_list` |
| `path` | string | no | - | Save path (auto-appends `.png`); omit to return base64 |

\* At least one of `title` / `hwnd` is required.

**Example**
```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"desktop_window_screenshot","arguments":{"title":"Notepad","path":"C:/Users/me/Desktop/notepad.png"}}}
```

---

### desktop_window_move

Move a window to the specified screen coordinates (Windows: `SetWindowPos`,
keeps size and z-order, shows the window). Get the `hwnd` from
`desktop_windows_list`.

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `hwnd` | integer | **yes** | - | Window handle from `desktop_windows_list` |
| `x` | integer | **yes** | - | Target X screen coordinate |
| `y` | integer | **yes** | - | Target Y screen coordinate |

**Example**
```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"desktop_window_move","arguments":{"hwnd":123456,"x":100,"y":100}}}
```
**Returns** `{"hwnd":123456,"x":100,"y":100,"moved":true}`

---

### desktop_window_resize

Resize a window to the specified `width` × `height` (Windows: `SetWindowPos`,
keeps position and z-order, shows the window).

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `hwnd` | integer | **yes** | - | Window handle from `desktop_windows_list` |
| `width` | integer | **yes** | - | New window width in pixels (> 0) |
| `height` | integer | **yes** | - | New window height in pixels (> 0) |

**Example**
```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"desktop_window_resize","arguments":{"hwnd":123456,"width":800,"height":600}}}
```
**Returns** `{"hwnd":123456,"width":800,"height":600,"resized":true}`

---

### desktop_window_info

Query detailed window information: title, visibility, minimized/maximized
state, window rect and client rect (screen coordinates), process id/name and
window class. Read-only.

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `hwnd` | integer | **yes** | - | Window handle from `desktop_windows_list` |

**Example**
```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"desktop_window_info","arguments":{"hwnd":123456}}}
```
**Returns**
```json
{"hwnd":123456,"title":"Notepad","visible":true,"minimized":false,"maximized":false,"window":{"x":100,"y":100,"width":800,"height":600},"client":{"x":104,"y":132,"width":792,"height":564},"process_id":1234,"process_name":"notepad.exe","class_name":"Notepad"}
```

---

### desktop_vision

Understand a screenshot with **your own** vision model (BYOK — OpenAI-compatible
or Anthropic native). Requires `NUPHUS_MCP_VISION_API_KEY` and
`NUPHUS_MCP_VISION_MODEL`; the protocol is auto-detected from the base URL (or
forced via `NUPHUS_MCP_VISION_PROVIDER`). See
[Vision & Local Models](#vision--local-models). If `path` is omitted the full
screen is captured first.

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `path` | string | no | - | Image file path (PNG); omit to capture the full screen first |
| `prompt` | string | no | - | Optional instruction for the vision model |

**Example**
```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"desktop_vision","arguments":{"prompt":"What is on the screen?"}}}
```
**Returns** `{"description":"<model text>"}` — when not configured:
`isError: true` + `NUPHUS_MCP_VISION_API_KEY required ...`.

---

### desktop_perceive

Locate UI elements in a screenshot with **local OCR (PaddleOCR)** + optional
**YOLO icon detection**. Downloads OCR models automatically on first run (see
[Vision & Local Models](#vision--local-models)). If `path` is omitted the full
screen is captured first. Returns elements with `id`, `kind` (text/button/
input/icon), `text`, `rect`, `center`, `confidence` and `source`
(ocr/yolo/both).

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `path` | string | no | - | Image file path (PNG); omit to capture the full screen first |

**Example**
```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"desktop_perceive","arguments":{}}}
```
**Returns**
```json
{"elements":[{"id":0,"kind":"button","text":"OK","rect":{"x":10,"y":10,"w":40,"h":20},"center":{"x":30,"y":20},"confidence":0.9,"source":"both"}, ...],"count":42,"ocr_count":30,"yolo_count":12,"yolo_available":true,"models_dir":"C:\\Users\\me\\AppData\\Roaming\\Nuphus\\models"}
```
When OCR models are missing and the download fails: `isError: true` with a
clear message and manual download instructions.

---

### desktop_mouse

Mouse operations: `click` / `double_click` / `hover` / `scroll` / `move`
require `(x, y)`. `position` is read-only: it returns the current cursor
position without moving the mouse.

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `action` | string | **yes** | - | One of `click`, `double_click`, `hover`, `scroll`, `position`, `move` |
| `x` | integer | no | 0 | X coordinate |
| `y` | integer | no | 0 | Y coordinate |
| `button` | string | no | - | Mouse button for `click`: `left`, `right`, `middle` |
| `clicks` | integer | no | 1 | Number of clicks (`click`) |
| `direction` | string | no | - | Scroll direction for `scroll`: `up`, `down` |
| `amount` | integer | no | 3 | Scroll ticks for `scroll` |

**Example — click**
```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"desktop_mouse","arguments":{"action":"click","x":100,"y":200,"button":"left"}}}
```

**Example — read cursor position (read-only)**
```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"desktop_mouse","arguments":{"action":"position"}}}
```
**Returns** `{"x":512,"y":384}`

---

### desktop_mouse_drag

Drag the mouse from a start coordinate to an end coordinate (captcha sliders,
slider validation, etc.).

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `start_x` | integer | **yes** | - | Start X coordinate |
| `start_y` | integer | **yes** | - | Start Y coordinate |
| `end_x` | integer | **yes** | - | End X coordinate |
| `end_y` | integer | **yes** | - | End Y coordinate |

**Example**
```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"desktop_mouse_drag","arguments":{"start_x":50,"start_y":300,"end_x":350,"end_y":300}}}
```

---

### desktop_input

Send text into a window (auto UTF-8 encoding), optionally followed by a key
press — an atomic operation. Normal text is typed directly; text longer than
500 characters should use the clipboard instead. Activate the target window
with `desktop_window_activate` first.

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `mode` | string | **yes** | - | `type`: input text; `hotkey`: press keys only |
| `hwnd` | integer | **yes** | - | Target window handle from `desktop_windows_list` |
| `text` | string | no | - | Text to type (`mode=type` required) |
| `send` | string | no | `"enter"` | Key to send after typing: `"enter"` (default), `"ctrl+enter"`, `"tab"`, or `"none"` to skip |
| `keys` | array<string> | no | - | Key combo to press (`mode=hotkey` required) |

**Example — type text and press Enter**
```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"desktop_input","arguments":{"mode":"type","hwnd":123456,"text":"hello world","send":"enter"}}}
```

**Example — hotkey**
```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"desktop_input","arguments":{"mode":"hotkey","hwnd":123456,"keys":["ctrl","s"]}}}
```

---

### desktop_clipboard_clean

Clear the system clipboard. MUST be called after pasting sensitive content
(passwords / tokens / verification codes) to prevent residue leakage.

⚠️ This tool is for clearing only — do NOT use it to read clipboard content.

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| *(none)* | | | | No arguments |

**Example**
```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"desktop_clipboard_clean","arguments":{}}}
```
**Returns** `{"cleared":true}`

---

### desktop_clipboard_write

Write long text (> 500 characters) to the clipboard for pasting. Normal text
should be typed directly with `desktop_input` — no clipboard needed. After
pasting, MUST call `desktop_clipboard_clean` to clear residue.

⚠️ Prohibited for passwords / sensitive data.

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `text` | string | **yes** | - | Text to write |

**Example**
```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"desktop_clipboard_write","arguments":{"text":"<long text>"}}}
```
**Returns** `{"written_chars":1234}`

---

## Browser Tools (22)

Browser tools operate a Chrome instance over CDP (`chromiumoxide`). The first
browser call launches a visible Chrome window; `browser_close` closes it and
releases resources. A 15-second timeout guards CDP operations (`navigate` /
`back` / `forward` get 30s; `wait_for` gets its timeout + 5s).

### browser_navigate

Open URL in browser.

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `url` | string | **yes** | - | URL to navigate to |

**Example**
```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"browser_navigate","arguments":{"url":"https://example.com"}}}
```
**Returns** navigation confirmation followed by a `── Page state ──` snapshot.

---

### browser_snapshot

Get a text snapshot of visible interactive elements using the Chrome
Accessibility Tree. Outputs `@N [role] "name"` format (e.g. `@1 [button]
"Submit"`). Falls back to DOM traversal if the AX tree is unavailable. Use the
`@N` refs for `browser_click` / `browser_type`.

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `full` | boolean | no | `false` | Include hidden elements too |
| `selector` | string | no | - | CSS selector to scope the snapshot (e.g. `'#quiz'`, `'.main-content'`); only elements within this subtree are numbered |

**Example**
```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"browser_snapshot","arguments":{"selector":"#main"}}}
```

---

### browser_exec

Execute a multi-step batch script in ONE CDP round trip. Use for form filling
and multi-click workflows. The script uses `window.__nuphus` helpers aliased as
`h`:

- `h.click('@N' | 'selector')`
- `h.fill('@N' | 'selector', text)`
- `h.scroll(px)`
- `h.wait(ms)`
- `h.extract('selector')`
- `h.snapshot()`

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `script` | string | **yes** | - | JS script using `window.__nuphus` helpers (aliased as `h`) |

**Example**
```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"browser_exec","arguments":{"script":"h.fill('@2', 'alice'); h.click('@5'); h.wait(800); h.snapshot();"}}}
```
**Returns** `[{op, ref, success, detail}]` per step.

---

### browser_click

Click element by CSS selector or ref ID from snapshot (e.g. `@1`, `@e0`,
`'button'`). CSS selector paths auto-wait for the element to appear and become
visible (up to 5s) before clicking.

Default left clicks are JS-synthesized (reliable, ignore overlays) but do NOT
produce user activation. Pass `trusted: true` to dispatch real CDP mouse
events (`isTrusted=true`) at the element's center. Right and middle clicks
always use trusted CDP events. Post-click snapshots default to off for
right/middle clicks so transient context menus remain open for the next call.

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `selector` | string | selector or `ref` | - | CSS selector or ref ID (e.g. `@1`, `@e0`, `'button'`) |
| `ref` | string | selector or `ref` | - | Snapshot ref ID; alias of `selector` |
| `trusted` | boolean | no | `false` | Dispatch real trusted CDP mouse events (produces user activation) instead of a JS click. Use for autoplay-gated media playback and gesture-gated features. |
| `button` | string | no | `left` | Mouse button: `left`, `right`, or `middle`. Right and middle are always trusted. |
| `snapshot` | boolean | no | button-dependent | Include a post-click snapshot; defaults to `true` for left and `false` for right/middle. |

**Example**
```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"browser_click","arguments":{"selector":"@1"}}}
```
```json
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"browser_click","arguments":{"selector":"button.play","trusted":true}}}
```
```json
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"browser_click","arguments":{"selector":".file-row","button":"right"}}}
```
**Returns** click confirmation; when `snapshot` is enabled, it is followed by a
`── Page state ──` snapshot.

---

### browser_type

Type text into an input field by CSS selector or ref ID from snapshot. CSS
selector paths auto-wait for the element to appear and become visible (up to
5s) before typing.

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `selector` | string | selector or `ref` | - | CSS selector or ref ID of input field (e.g. `@1`, `@e0`) |
| `ref` | string | selector or `ref` | - | Snapshot ref ID; alias of `selector` |
| `text` | string | **yes** | - | Text to type |

**Example**
```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"browser_type","arguments":{"selector":"@2","text":"alice@example.com"}}}
```

---

### browser_scroll

Scroll page up/down by N pixels.

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `direction` | string | **yes** | - | Scroll direction: `up`, `down` |
| `amount` | integer | no | `500` | Pixels to scroll |

**Example**
```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"browser_scroll","arguments":{"direction":"down","amount":800}}}
```

---

### browser_extract

Extract readable text from the current page (strips nav/ads).

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `max_chars` | integer | no | `8000` | Max characters to extract |

**Example**
```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"browser_extract","arguments":{"max_chars":4000}}}
```

---

### browser_screenshot

Screenshot the current browser page.

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `path` | string | **yes** | - | Existing-parent save path for the PNG file |

**Example**
```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"browser_screenshot","arguments":{"path":"page.png"}}}
```
**Returns** the saved path and PNG byte count.

---

### browser_close

Close browser and free resources.

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| *(none)* | | | | No arguments |

**Example**
```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"browser_close","arguments":{}}}
```
**Returns** `Browser closed`

---

### browser_evaluate

Execute arbitrary JavaScript in the current page.

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `script` | string | **yes** | - | JavaScript code |

**Example**
```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"browser_evaluate","arguments":{"script":"document.title"}}}
```

---

### browser_back

Navigate back in browser history.

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| *(none)* | | | | No arguments |

**Example**
```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"browser_back","arguments":{}}}
```

---

### browser_forward

Navigate forward in browser history.

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| *(none)* | | | | No arguments |

**Example**
```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"browser_forward","arguments":{}}}
```

---

### browser_wait_for

Wait for a CSS selector to reach the given state on the page (up to timeout).
Note: `browser_click` / `browser_type` CSS paths already auto-wait
(presence + visible, 5s), so explicit waits are usually only needed for custom
states or longer delays.

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `selector` | string | **yes** | - | CSS selector to wait for |
| `timeout_ms` | integer | no | `5000` | Max wait time in ms |
| `state` | string | no | `"attached"` | Target state: `attached`, `visible`, `hidden` |

**Example**
```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"browser_wait_for","arguments":{"selector":".result","state":"visible","timeout_ms":10000}}}
```

---

### browser_cookies_get

Get all cookies for the current page.

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| *(none)* | | | | No arguments |

**Example**
```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"browser_cookies_get","arguments":{}}}
```

---

### browser_cookies_set

Set a cookie for the current domain.

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `name` | string | **yes** | - | Cookie name |
| `value` | string | **yes** | - | Cookie value |
| `domain` | string | no | current domain | Cookie domain |
| `path` | string | no | - | Cookie path |

**Example**
```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"browser_cookies_set","arguments":{"name":"theme","value":"dark"}}}
```

---

### browser_import_cookies

Import cookies from the user's Chrome profile into the current browser
session. Requires a cookie data source registered by the host environment
(Nuphus); on a bare `nuphus-mcp` install this may be unavailable and will
return an explanatory error.

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `domain` | string | no | - | Optional domain filter |

**Example**
```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"browser_import_cookies","arguments":{"domain":"example.com"}}}
```

---

### browser_upload

Upload a file to a file input element using the DataTransfer trick. The file
must exist on disk.

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `selector` | string | **yes** | - | CSS selector or `@N` ref of file input |
| `file_path` | string | **yes** | - | Absolute path to the file to upload |

**Example**
```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"browser_upload","arguments":{"selector":"input[type=file]","file_path":"C:/Users/me/Desktop/report.pdf"}}}
```

---

### browser_drag_files

Drag one or more existing local files or directories onto any browser element
using native Chrome DevTools drag events. This does not require an
`input[type=file]` element, does not base64-encode contents, and therefore is
not subject to the MCP request-line size limit.

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `selector` | string | selector or `ref` | - | CSS selector or snapshot ref for the drop target |
| `ref` | string | selector or `ref` | - | Snapshot ref ID; alias of `selector` |
| `file_paths` | string[] | **yes** | - | Absolute paths of existing local files or directories |

**Example**
```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"browser_drag_files","arguments":{"selector":".explorer-viewlet","file_paths":["/Users/me/report.pdf","/Users/me/assets"]}}}
```

---

### browser_list_downloads

List files in the browser download directory.

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| *(none)* | | | | No arguments |

**Example**
```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"browser_list_downloads","arguments":{}}}
```

---

### browser_new_tab

Open a new browser tab.

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `url` | string | no | - | URL to open in the new tab |

**Example**
```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"browser_new_tab","arguments":{"url":"https://example.com"}}}
```

---

### browser_list_tabs

List all open tabs with indices, URLs, and titles. Indices reflect the current
tab ordering and may change when tabs are opened or closed.

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| *(none)* | | | | No arguments |

**Example**
```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"browser_list_tabs","arguments":{}}}
```

---

### browser_switch_tab

Switch to a tab by index and bring it to the front in the visible Chrome window.

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `index` | integer | **yes** | - | Tab index from `browser_list_tabs` |

**Example**
```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"browser_switch_tab","arguments":{"index":1}}}
```

---

## End-to-End Example

A complete stdio session: initialize → list tools → screenshot the screen →
open a page → fill and submit a form.

```
→ {"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"my-agent","version":"1.0.0"}}}
← {"jsonrpc":"2.0","id":0,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{"listChanged":false}},"serverInfo":{"name":"nuphus-mcp","version":"0.1.0"},...}}

→ {"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}
← {"jsonrpc":"2.0","id":1,"result":{"tools":[ ... 37 tools ... ]}}

→ {"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"desktop_screenshot","arguments":{}}}
← {"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":"{\"format\":\"png\",\"data\":\"...\",\"width\":1920,\"height\":1080}"}]}}

→ {"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"browser_navigate","arguments":{"url":"https://example.com"}}}
← {"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"Navigated to: https://example.com\n\n── Page state ──\n@1 [link] \"More information\"..."}]}}

→ {"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"browser_exec","arguments":{"script":"h.click('@1'); h.wait(500); h.snapshot();"}}}
← {"jsonrpc":"2.0","id":4,"result":{"content":[{"type":"text","text":"[{\"op\":\"click\",\"ref\":\"@1\",\"success\":true,\"detail\":\"...\"},...]"}]}}
```

---

*Generated from the current `tools/list` schema of `nuphus-mcp`. Only the 37
tools listed above are exposed by this version.*
