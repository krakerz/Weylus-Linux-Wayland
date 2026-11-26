enum LogLevel {
    ERROR = 0,
    WARN,
    INFO,
    DEBUG,
    TRACE,
}

let log_pre: HTMLPreElement;
let log_level: LogLevel = LogLevel.ERROR;
let no_log_messages: boolean = true;
// warnings and errors are mirrored to the server log: iPad Safari has no console without a Mac
let log_socket: WebSocket | null = null;

let fps_out: HTMLOutputElement;
let frame_count = 0;
let last_fps_calc: number = performance.now();
let stats_overlay: HTMLElement;

function now_unix_ms(): number {
    return performance.timeOrigin + performance.now();
}

// Client-side numbers for the stats overlay. The counters are per interval and reset by frame_rate_stats().
class VideoStats {
    bytes = 0;
    presented = 0;
    dropped = 0;
    dropped_total = 0;
    decode_ms: number[] = [];
    latency_ms: number[] = [];
    codec = "";
    width = 0;
    height = 0;
    // unix ms of pts 0 of the current encoder
    epoch: number | null = null;
    // server clock minus client clock in ms
    offset: number | null = null;
    rtt: number | null = null;
    pings: { rtt: number, offset: number }[] = [];
    submitted = new Map<number, number>();

    on_submit(ts_us: number) {
        let now = performance.now();
        this.submitted.set(ts_us, now);
        if (this.submitted.size > 64) {
            for (let [k, t] of this.submitted) {
                if (now - t > 2000)
                    this.submitted.delete(k);
            }
        }
    }

    on_decoded(ts_us: number) {
        let t = this.submitted.get(ts_us);
        if (t === undefined)
            return;
        this.decode_ms.push(performance.now() - t);
        this.submitted.delete(ts_us);
    }

    // call before frame.close()
    on_present(frame: VideoFrame) {
        this.presented++;
        this.width = frame.displayWidth;
        this.height = frame.displayHeight;
        if (this.epoch != null && this.offset != null) {
            let lat = now_unix_ms() + this.offset - (this.epoch + frame.timestamp / 1000);
            // guards against clock jumps and a stale epoch
            if (lat >= 0 && lat < 10000)
                this.latency_ms.push(lat);
        }
    }

    on_pong(msg: { client_ms: number, server_ms: number }) {
        let t = now_unix_ms();
        let rtt = t - msg.client_ms;
        let offset = msg.server_ms - (msg.client_ms + t) / 2;
        this.pings.push({ rtt, offset });
        if (this.pings.length > 8)
            this.pings.shift();
        let best = this.pings[0];
        for (let p of this.pings) {
            if (p.rtt < best.rtt)
                best = p;
        }
        this.offset = best.offset;
        this.rtt = rtt;
    }

    ping(ws: WebSocket | null) {
        if (ws && ws.readyState == WebSocket.OPEN)
            ws.send(JSON.stringify({ "Ping": now_unix_ms() }));
    }
}

const video_stats = new VideoStats();

let check_video: HTMLInputElement;

function run(level: string) {
    window.onload = () => {
        log_pre = document.getElementById("log") as HTMLPreElement;
        log_pre.textContent = "";
        log_level = LogLevel[level];
        fps_out = document.getElementById("fps") as HTMLOutputElement;
        check_video = document.getElementById("enable_video") as HTMLInputElement;
        window.addEventListener("error", (e: ErrorEvent | Event | UIEvent) => {
            if ((e as ErrorEvent).error) {
                let err = e as ErrorEvent;
                log(LogLevel.ERROR, err.filename + ":L" + err.lineno + ":" + err.colno + ": " + err.message + " Error object: " + JSON.stringify(err.error));
            } else if ((e as Event | UIEvent).target) {
                let ev = e as Event;
                let src = (e.target as any).src;
                log(LogLevel.ERROR, "Failed to obtain resource, target: " + ev.target + " type: " + ev.type + " src: " + src + " Error object: " + JSON.stringify(ev));
            } else {
                log(LogLevel.WARN, "Got unknown event: " + JSON.stringify(e));
            }
            return false;
        }, true)
        init();
    };
}

function log(level: LogLevel, msg: string) {
    if (level <= LogLevel.WARN && log_socket && log_socket.readyState == WebSocket.OPEN)
        log_socket.send(JSON.stringify({ "ClientLog": LogLevel[level] + ": " + msg }));
    if (level > log_level)
        return;

    if (level == LogLevel.TRACE)
        console.trace(msg);
    else if (level == LogLevel.DEBUG)
        console.debug(msg);
    else if (level == LogLevel.INFO)
        console.info(msg);
    else if (level == LogLevel.WARN)
        console.warn(msg);
    else if (level == LogLevel.ERROR)
        console.error(msg);

    if (no_log_messages) {
        no_log_messages = false;
        document.getElementById("log_section").classList.remove("hide");
    }
    log_pre.textContent += LogLevel[level] + ": " + msg + "\n";
}

function frame_rate_scale(x: number) {
    return Math.pow(x / 100, 1.5);
}

function frame_rate_scale_inv(x: number) {
    return 100 * Math.pow(x, 2 / 3);
}


function calc_max_video_resolution(scale: number) {
    return [
        Math.round(scale * window.innerWidth * window.devicePixelRatio),
        Math.round(scale * window.innerHeight * window.devicePixelRatio)
    ];
}

function fresh_canvas() {
    let canvas_old = document.getElementById("canvas");
    let canvas = document.createElement("canvas");
    canvas.id = canvas_old.id;
    canvas_old.classList.forEach((cls) => canvas.classList.add(cls));
    canvas_old.replaceWith(canvas);
    return canvas;
}

class Rect {
    x: number;
    y: number;
    w: number;
    h: number;
}

class CustomInputAreas {
    mouse: Rect;
    touch: Rect;
    pen: Rect;
}

function bitrate_label(v: number) {
    return v > 0 ? v.toFixed(1) + " Mbps" : "Auto";
}

// codec ids as used in the second header byte of video messages
const CODEC_IDS = ["h264", "hevc", "av1"];
const CODEC_LABELS: { [k: string]: string } = { "h264": "H.264", "hevc": "H.265", "av1": "AV1" };

// KeyboardEvent.code values the shortcut bar may send (must match the uinput map_key test list)
const COMBO_CODES: Set<string> = new Set([
    ..."ABCDEFGHIJKLMNOPQRSTUVWXYZ".split("").map((c) => "Key" + c),
    ..."0123456789".split("").map((c) => "Digit" + c),
    ...Array.from({ length: 12 }, (_, i) => "F" + (i + 1)),
    "BracketLeft", "BracketRight", "Minus", "Equal", "Comma", "Period", "Slash", "Backslash",
    "Semicolon", "Quote", "Backquote", "Space", "Tab", "Enter", "Escape", "Backspace", "Delete",
    "Insert", "Home", "End", "PageUp", "PageDown", "ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight",
    "ControlLeft", "ShiftLeft", "AltLeft", "MetaLeft",
]);

const COMBO_MODIFIERS = ["ControlLeft", "ShiftLeft", "AltLeft", "MetaLeft"];
const COMBO_ALIASES: { [k: string]: string } = {
    "ctrl": "ControlLeft", "control": "ControlLeft",
    "shift": "ShiftLeft",
    "alt": "AltLeft", "option": "AltLeft",
    "meta": "MetaLeft", "super": "MetaLeft", "win": "MetaLeft", "cmd": "MetaLeft",
    "[": "BracketLeft", "]": "BracketRight", "-": "Minus", "=": "Equal", ",": "Comma", ".": "Period",
    "/": "Slash", "\\": "Backslash", ";": "Semicolon", "'": "Quote", "`": "Backquote",
    "space": "Space", "tab": "Tab", "enter": "Enter", "return": "Enter", "esc": "Escape", "escape": "Escape",
    "backspace": "Backspace", "del": "Delete", "delete": "Delete", "ins": "Insert", "insert": "Insert",
    "home": "Home", "end": "End", "pgup": "PageUp", "pageup": "PageUp", "pgdn": "PageDown", "pagedown": "PageDown",
    "up": "ArrowUp", "down": "ArrowDown", "left": "ArrowLeft", "right": "ArrowRight",
};

// "Ctrl+Shift+Z" -> ["ControlLeft", "ShiftLeft", "KeyZ"]: modifiers in a fixed order, then at most one key.
// null when anything is unknown. DOM-free.
function parse_combo(s: string): string[] | null {
    let mods = new Set<string>();
    let key: string | null = null;
    for (let raw of s.split("+")) {
        let tok = raw.trim();
        if (tok == "")
            return null;
        let lower = tok.toLowerCase();
        let code: string | undefined;
        if (Object.prototype.hasOwnProperty.call(COMBO_ALIASES, lower))
            code = COMBO_ALIASES[lower];
        else if (/^[a-z]$/.test(lower))
            code = "Key" + lower.toUpperCase();
        else if (/^[0-9]$/.test(lower))
            code = "Digit" + lower;
        else if (/^f([1-9]|1[0-2])$/.test(lower))
            code = lower.toUpperCase();
        else if (COMBO_CODES.has(tok))
            code = tok;
        if (code === undefined)
            return null;
        if (COMBO_MODIFIERS.includes(code)) {
            mods.add(code);
        } else {
            if (key !== null)
                return null;
            key = code;
        }
    }
    let out = COMBO_MODIFIERS.filter((m) => mods.has(m));
    if (key !== null)
        out.push(key);
    return out.length ? out : null;
}

interface Shortcut { label: string; codes: string[]; hold: boolean; icon?: string }

// One button per line: "label = [hold ]combo" or a bare "[hold ]combo". DOM-free.
function parse_custom_shortcuts(text: string): { shortcuts: Shortcut[], errors: string[] } {
    let shortcuts: Shortcut[] = [];
    let errors: string[] = [];
    text.split("\n").forEach((raw, i) => {
        let line = raw.trim();
        if (line == "" || line.startsWith("#"))
            return;
        let label: string | null = null;
        let rest = line;
        let eq = line.indexOf("=");
        // a lone "=" or "Ctrl+=" is a combo, not a label separator
        if (eq > 0 && line.slice(0, eq).trim() != "") {
            label = line.slice(0, eq).trim();
            rest = line.slice(eq + 1).trim();
        }
        let hold = false;
        let m = /^hold\s+(.*)$/i.exec(rest);
        if (m) {
            hold = true;
            rest = m[1].trim();
        }
        let codes = parse_combo(rest);
        if (!codes && label !== null) {
            // maybe the whole line is a combo containing "=" (e.g. "Ctrl+=")
            let whole = /^hold\s+(.*)$/i.exec(line);
            let alt = parse_combo(whole ? whole[1] : line);
            if (alt) {
                codes = alt;
                hold = !!whole;
                label = null;
                rest = whole ? whole[1].trim() : line;
            }
        }
        if (!codes) {
            errors.push("line " + (i + 1) + ": unknown key combo \"" + rest + "\"");
            return;
        }
        shortcuts.push({ label: label ?? rest, codes, hold });
    });
    return { shortcuts, errors };
}

// [label, combo, hold, icon?]
const SHORTCUT_PRESETS: { [k: string]: [string, string, boolean, string?][] } = {
    "krita": [
        ["Undo", "Ctrl+Z", false, "undo"],
        ["Redo", "Ctrl+Shift+Z", false, "redo"],
        ["Eraser", "E", false, "eraser"],
        ["Smaller", "[", false, "smaller"],
        ["Bigger", "]", false, "bigger"],
        ["Pan", "Space", true, "pan"],
        ["Pick", "Ctrl", true, "picker"],
    ],
    "gimp": [
        ["Undo", "Ctrl+Z", false, "undo"],
        ["Redo", "Ctrl+Y", false, "redo"],
        ["Brush", "P", false, "pencil"],
        ["Eraser", "Shift+E", false, "eraser"],
        ["Smaller", "[", false, "smaller"],
        ["Bigger", "]", false, "bigger"],
        ["Pan", "Space", true, "pan"],
        ["Pick", "Ctrl", true, "picker"],
    ],
    "xournalpp": [
        ["Undo", "Ctrl+Z", false, "undo"],
        ["Redo", "Ctrl+Y", false, "redo"],
        ["Pen", "Ctrl+Shift+P", false, "pencil"],
        ["Eraser", "Ctrl+Shift+E", false, "eraser"],
        ["Highlighter", "Ctrl+Shift+H", false, "highlighter"],
        ["Hand", "Ctrl+Shift+A", false, "pan"],
    ],
    "generic": [
        ["Undo", "Ctrl+Z", false, "undo"],
        ["Redo", "Ctrl+Shift+Z", false, "redo"],
        ["Ctrl", "Ctrl", true],
        ["Shift", "Shift", true],
        ["Alt", "Alt", true],
        ["Space", "Space", true],
        ["Esc", "Esc", false],
        ["Del", "Delete", false],
    ],
    "none": [],
};

class Settings {
    webSocket: WebSocket;
    checks: Map<string, HTMLInputElement>;
    capturable_select: HTMLSelectElement;
    frame_rate_input: HTMLInputElement;
    frame_rate_output: HTMLOutputElement;
    scale_video_input: HTMLInputElement;
    scale_video_output: HTMLOutputElement;
    range_min_pressure: HTMLInputElement;
    pen_pressure_mode: HTMLSelectElement;
    reselect_capture = false;
    bitrate_input: HTMLInputElement;
    bitrate_output: HTMLOutputElement;
    codec_select: HTMLSelectElement;
    codec_output: HTMLOutputElement;
    decodable: Set<string> = new Set(["h264"]);
    codecs_probed: Promise<void>;
    last_codec_label: string | null = null;
    // codecs that were given up on this session (never fall back twice for the same one)
    codec_fell_back: Set<string> = new Set();
    codec_fallback_note: string | null = null;
    // the codec the user picked before a fallback to Auto: that is what stays in localStorage
    codec_saved_choice: string | null = null;
    client_name_input: HTMLInputElement;
    shortcut_preset: HTMLSelectElement;
    shortcut_side: HTMLSelectElement;
    shortcut_custom: HTMLTextAreaElement;
    screen_pick_timer: number | undefined;
    visible: boolean;
    custom_input_areas: CustomInputAreas;
    settings: HTMLElement;

    constructor(webSocket: WebSocket) {
        this.webSocket = webSocket;
        this.checks = new Map<string, HTMLInputElement>();
        this.capturable_select = document.getElementById("window") as HTMLSelectElement;
        this.frame_rate_input = document.getElementById("frame_rate") as HTMLInputElement;
        this.frame_rate_input.min = frame_rate_scale_inv(0).toString();
        this.frame_rate_input.max = frame_rate_scale_inv(120).toString();
        this.frame_rate_output = this.frame_rate_input.nextElementSibling as HTMLOutputElement;
        this.scale_video_input = document.getElementById("scale_video") as HTMLInputElement;
        this.scale_video_output = this.scale_video_input.nextElementSibling as HTMLOutputElement;
        this.range_min_pressure = document.getElementById("min_pressure") as HTMLInputElement;
        this.pen_pressure_mode = document.getElementById("pen_pressure_mode") as HTMLSelectElement;
        this.bitrate_input = document.getElementById("bitrate") as HTMLInputElement;
        this.bitrate_output = this.bitrate_input.nextElementSibling as HTMLOutputElement;
        this.codec_select = document.getElementById("codec") as HTMLSelectElement;
        this.codec_output = document.getElementById("active_codec") as HTMLOutputElement;
        this.client_name_input = document.getElementById("client_name") as HTMLInputElement;
        this.shortcut_preset = document.getElementById("shortcut_preset") as HTMLSelectElement;
        this.shortcut_side = document.getElementById("shortcut_side") as HTMLSelectElement;
        this.shortcut_custom = document.getElementById("shortcut_custom") as HTMLTextAreaElement;
        this.codecs_probed = this.probe_codecs();
        this.frame_rate_input.oninput = () => {
            this.frame_rate_output.value = Math.round(frame_rate_scale(this.frame_rate_input.valueAsNumber)).toString();
        }
        this.scale_video_input.oninput = () => {
            let [w, h] = calc_max_video_resolution(this.scale_video_input.valueAsNumber)
            this.scale_video_output.value = w + "x" + h
        }
        this.bitrate_input.oninput = () => {
            this.bitrate_output.value = bitrate_label(this.bitrate_input.valueAsNumber);
        }
        this.range_min_pressure.oninput = () => this.update_pressure_output();
        this.visible = true;

        // Settings UI
        this.settings = document.getElementById("settings");
        this.settings.onclick = (e) => e.stopPropagation();
        let handle = document.getElementById("handle");

        // Settings elements
        this.settings.querySelectorAll("input[type=checkbox]").forEach(
            (elem, _key, _parent) => this.checks.set(elem.id, elem as HTMLInputElement)
        );

        this.load_settings();

        // event handling

        // client only
        handle.onclick = () => { this.toggle() };
        this.checks.get("lefty").onchange = (e) => {
            if ((e.target as HTMLInputElement).checked)
                this.settings.classList.add("lefty");
            else
                this.settings.classList.remove("lefty");
            this.save_settings();
        }

        document.getElementById("vanish").onclick = () => {
            this.settings.classList.add("vanish");
        }

        this.checks.get("stretch").onchange = () => {
            stretch_video();
            this.save_settings();
        };

        this.checks.get("enable_debug_overlay").onchange = (e) => {
            let enabled = (e.target as HTMLInputElement).checked;
            if (enabled) {
                debug_overlay.classList.remove("hide");
            } else {
                debug_overlay.classList.add("hide");
            }
            this.save_settings();
        };

        this.checks.get("enable_stats_overlay").onchange = (e) => {
            stats_overlay.classList.toggle("hide", !(e.target as HTMLInputElement).checked);
            this.save_settings();
        };

        this.checks.get("enable_shortcuts").onchange = () => {
            this.save_settings();
            shortcut_bar?.rebuild();
        };
        this.shortcut_preset.onchange = () => {
            this.save_settings();
            shortcut_bar?.rebuild();
        };
        this.shortcut_side.onchange = () => {
            this.save_settings();
            shortcut_bar?.rebuild();
        };
        // fires on blur
        this.shortcut_custom.onchange = () => {
            this.save_settings();
            shortcut_bar?.rebuild();
        };

        this.checks.get("enable_video").onchange = (e) => {
            let enabled = (e.target as HTMLInputElement).checked;
            document.getElementById("video").classList.toggle("vanish", !enabled);
            document.getElementById("canvas").classList.toggle("vanish", enabled);
            this.save_settings();
            if (enabled) {
                this.webSocket.send('"ResumeVideo"');
            } else {
                this.webSocket.send('"PauseVideo"');
            }
        }

        let upd_pointer = () => {
            this.save_settings();
            new PointerHandler(this.webSocket);
        }
        this.checks.get("enable_mouse").onchange = upd_pointer;
        this.checks.get("enable_stylus").onchange = upd_pointer;
        this.checks.get("enable_touch").onchange = upd_pointer;

        this.checks.get("energysaving").onchange = (e) => {
            this.save_settings();
            this.toggle_energysaving((e.target as HTMLInputElement).checked);
        };

        this.checks.get("enable_custom_input_areas").onchange = () => {
            this.save_settings();
        };

        this.frame_rate_input.onchange = () => this.save_settings();
        this.range_min_pressure.onchange = () => this.save_settings();
        this.pen_pressure_mode.onchange = () => {
            // a fixed pressure of 0 would draw nothing
            if (this.pen_pressure_mode.value == "fixed" && this.range_min_pressure.valueAsNumber == 0) {
                this.range_min_pressure.value = "1";
                this.update_pressure_output();
            }
            this.save_settings();
        };

        // server
        let upd_server_config = () => { this.save_settings(); this.send_server_config() };
        this.checks.get("uinput_support").onchange = upd_server_config;
        this.checks.get("capture_cursor").onchange = () => {
            this.save_settings();
            // the portal decides the cursor mode per session: start a new one (the screen picker shows again)
            this.webSocket.send(JSON.stringify({ "SetCaptureCursor": this.checks.get("capture_cursor").checked }));
            this.reselect_capture = true;
            this.await_screen_pick();
            this.webSocket.send('"RefreshCapturableList"');
        };
        this.scale_video_input.onchange = upd_server_config;
        this.bitrate_input.onchange = upd_server_config;
        this.codec_select.onchange = () => {
            // an explicit change by the user replaces the session fallback
            this.codec_saved_choice = null;
            this.codec_fallback_note = null;
            upd_server_config();
        };
        this.client_name_input.onchange = upd_server_config;
        this.frame_rate_input.onchange = upd_server_config;

        document.getElementById("refresh").onclick = () => {
            // the new portal session renames its stream: reselect it so the video resumes
            this.reselect_capture = true;
            this.await_screen_pick();
            this.webSocket.send('"RefreshCapturableList"');
        };
        document.getElementById("custom_input_areas").onclick = () => {
            this.webSocket.send('"ChooseCustomInputAreas"');
        };
        this.capturable_select.onchange = () => this.send_server_config();
    }

    update_pressure_output() {
        (this.range_min_pressure.nextElementSibling as HTMLOutputElement).value =
            this.range_min_pressure.valueAsNumber.toFixed(2);
    }

    send_server_config() {
        let config = new Object(null);
        config["capturable_id"] = Number(this.capturable_select.value);
        for (const key of [
            "uinput_support",
            "capture_cursor"])
            config[key] = this.checks.get(key).checked;
        let [w, h] = calc_max_video_resolution(this.scale_video_input.valueAsNumber);
        config["max_width"] = w;
        config["max_height"] = h;
        config["frame_rate"] = frame_rate_scale(this.frame_rate_input.valueAsNumber);
        config["bitrate_mbps"] = this.bitrate_input.valueAsNumber;
        config["codecs"] = this.codec_list();
        if (this.client_name_input.value)
            config["client_name"] = this.client_name_input.value;
        this.webSocket.send(JSON.stringify({ "Config": config }));
    }

    probe_codecs(): Promise<void> {
        if (typeof VideoDecoder == "undefined")
            return Promise.resolve();
        const candidates: [string, string][] = [
            ["h264", "avc1.640033"],
            ["hevc", "hvc1.1.6.L153.B0"],
            ["av1", "av01.0.13M.08"],
        ];
        return Promise.all(candidates.map(([name, codec]) =>
            VideoDecoder.isConfigSupported({
                codec: codec,
                codedWidth: 1920,
                codedHeight: 1080,
                optimizeForLatency: true,
                hardwareAcceleration: "prefer-hardware",
            }).then((s) => s.supported ? name : null).catch(() => null)
        )).then((names) => {
            for (const n of names)
                if (n)
                    this.decodable.add(n);
        });
    }

    // codecs to request, best first
    codec_list(): string[] {
        let v = this.codec_select.value;
        if (v == "auto")
            return ["av1", "hevc", "h264"].filter(c => c == "h264" || this.decodable.has(c));
        // an explicit choice is not filtered, so it can override a wrong probe
        if (!this.decodable.has(v))
            log(LogLevel.WARN, "Selected codec " + v + " was not reported as decodable by this browser");
        return v == "h264" ? ["h264"] : [v, "h264"];
    }

    show_active_codec(id: number) {
        const names = CODEC_LABELS;
        let label = names[CODEC_IDS[id]] ?? "?";
        if (this.codec_fallback_note && this.codec_select.value == "auto")
            this.codec_output.value = "Auto: " + label + " (" + this.codec_fallback_note + ")";
        else
            this.codec_output.value = label;
        if (label == this.last_codec_label)
            return;
        this.last_codec_label = label;
        log(LogLevel.INFO, "Active video codec: " + label);
        let wanted = names[this.codec_list()[0]];
        if (label != wanted)
            log(LogLevel.WARN, "Server uses " + label + " instead of " + wanted);
    }

    // The user explicitly chose `name` (not h264/auto) and it can't be decoded here: switch to Auto and
    // save it, so later connects don't retry the codec. Returns whether it switched.
    try_codec_fallback(name: string, reason: string): boolean {
        const v = this.codec_select.value;
        if (v == "auto" || v == "h264" || v != name || this.codec_fell_back.has(name))
            return false;
        this.codec_fell_back.add(name);
        const label = CODEC_LABELS[name] ?? name;
        this.codec_saved_choice = null;
        this.codec_select.value = "auto";
        this.save_settings();
        this.codec_fallback_note = label + " unsupported";
        this.codec_output.value = "Auto (" + this.codec_fallback_note + ")";
        log(LogLevel.WARN, "Codec fallback reason: " + reason);
        log(LogLevel.WARN, "Codec " + label + " can't be decoded on this device, switched to Auto");
        this.send_server_config();
        return true;
    }

    save_settings() {
        let settings = Object(null);
        for (const [key, elem] of this.checks.entries())
            settings[key] = elem.checked;
        settings["frame_rate"] = frame_rate_scale(this.frame_rate_input.valueAsNumber).toString();
        settings["scale_video"] = this.scale_video_input.value;
        settings["min_pressure"] = this.range_min_pressure.value;
        settings["pen_pressure_mode"] = this.pen_pressure_mode.value;
        settings["bitrate"] = this.bitrate_input.value;
        settings["codec"] = this.codec_saved_choice ?? this.codec_select.value;
        settings["custom_input_areas"] = this.custom_input_areas;
        settings["client_name"] = this.client_name_input.value;
        settings["shortcut_preset"] = this.shortcut_preset.value;
        settings["shortcut_side"] = this.shortcut_side.value;
        settings["shortcut_custom"] = this.shortcut_custom.value;
        localStorage.setItem("settings", JSON.stringify(settings));
    }

    load_settings() {
        let settings_string = localStorage.getItem("settings");
        if (settings_string === null) {
            this.frame_rate_input.value = frame_rate_scale_inv(30).toString();
            this.frame_rate_output.value = (30).toString();
            let [w, h] = calc_max_video_resolution(this.scale_video_input.valueAsNumber)
            this.scale_video_output.value = w + "x" + h;
            this.bitrate_output.value = bitrate_label(this.bitrate_input.valueAsNumber);
            this.update_pressure_output();
            return;
        }
        try {
            let settings = JSON.parse(settings_string);
            for (const [key, elem] of this.checks.entries()) {
                if (typeof settings[key] === "boolean")
                    elem.checked = settings[key];
            }
            let upd_limit = settings["frame_rate"];
            if (upd_limit)
                this.frame_rate_input.value = frame_rate_scale_inv(upd_limit).toString();
            else
                this.frame_rate_input.value = frame_rate_scale_inv(30).toString();
            this.frame_rate_output.value = Math.round(frame_rate_scale(this.frame_rate_input.valueAsNumber)).toString();

            let scale_video = settings["scale_video"];
            if (scale_video)
                this.scale_video_input.value = scale_video;
            let [w, h] = calc_max_video_resolution(this.scale_video_input.valueAsNumber);
            this.scale_video_output.value = w + "x" + h;

            let min_pressure = settings["min_pressure"];
            if (min_pressure)
                this.range_min_pressure.value = min_pressure;
            this.update_pressure_output();

            let pen_pressure_mode = settings["pen_pressure_mode"];
            if (pen_pressure_mode === "device" || pen_pressure_mode === "fixed")
                this.pen_pressure_mode.value = pen_pressure_mode;

            let bitrate = settings["bitrate"];
            if (bitrate)
                this.bitrate_input.value = bitrate;
            this.bitrate_output.value = bitrate_label(this.bitrate_input.valueAsNumber);

            let codec = settings["codec"];
            if (codec === "auto" || codec === "h264" || codec === "hevc" || codec === "av1")
                this.codec_select.value = codec;

            this.custom_input_areas = settings["custom_input_areas"];

            if (this.checks.get("lefty").checked) {
                this.settings.classList.add("lefty");
            }

            if (!this.checks.get("enable_video").checked || this.checks.get("energysaving").checked) {
                this.checks.get("enable_video").checked = false;
                if (this.checks.get("energysaving").checked)
                    this.checks.get("enable_video").disabled = true;
                document.getElementById("video").classList.add("vanish");
                document.getElementById("canvas").classList.remove("vanish");
            }

            if (this.checks.get("energysaving").checked) {
                this.toggle_energysaving(true);
            }

            if (this.checks.get("enable_debug_overlay").checked) {
                debug_overlay.classList.remove("hide");
            }

            if (this.checks.get("enable_stats_overlay").checked) {
                stats_overlay.classList.remove("hide");
            }


            if (document.getElementById("custom_input_areas").classList.contains("hide")) {
                this.checks.get("enable_custom_input_areas").checked = false;
            }

            let shortcut_preset = settings["shortcut_preset"];
            if (typeof shortcut_preset == "string" && Object.prototype.hasOwnProperty.call(SHORTCUT_PRESETS, shortcut_preset))
                this.shortcut_preset.value = shortcut_preset;
            let shortcut_side = settings["shortcut_side"];
            if (shortcut_side === "left" || shortcut_side === "right")
                this.shortcut_side.value = shortcut_side;
            let shortcut_custom = settings["shortcut_custom"];
            if (typeof shortcut_custom == "string")
                this.shortcut_custom.value = shortcut_custom;

            let client_name = settings["client_name"];
            if (client_name)
                this.client_name_input.value = client_name;

        } catch {
            log(LogLevel.DEBUG, "Failed to load settings.")
            return;
        }
    }

    stretched_video() {
        return this.checks.get("stretch").checked
    }

    pointer_types() {
        let ptrs = [];
        if (this.checks.get("enable_mouse").checked)
            ptrs.push("mouse");
        if (this.checks.get("enable_stylus").checked)
            ptrs.push("pen");
        if (this.checks.get("enable_touch").checked)
            ptrs.push("touch");
        return ptrs;
    }

    toggle() {
        this.settings.classList.toggle("hide");
        this.visible = !this.visible;
    }

    await_screen_pick() {
        const b = document.getElementById("refresh") as HTMLButtonElement;
        b.disabled = true;
        b.textContent = "Pick on the PC…";
        // a cancelled or failed pick sends no list: don't leave the button stuck
        window.clearTimeout(this.screen_pick_timer);
        this.screen_pick_timer = window.setTimeout(() => this.reset_screen_pick(), 60000);
    }

    reset_screen_pick() {
        window.clearTimeout(this.screen_pick_timer);
        const b = document.getElementById("refresh") as HTMLButtonElement;
        b.disabled = false;
        b.textContent = "Change screen";
    }

    onCapturableList(window_names: string[]) {
        this.reset_screen_pick();
        let current_selection = undefined;
        if (this.capturable_select.selectedOptions[0])
            current_selection = this.capturable_select.selectedOptions[0].textContent;
        let new_index: number;
        this.capturable_select.innerText = "";
        window_names.forEach((name, i) => {
            let option = document.createElement("option");
            option.value = String(i);
            option.innerText = name;
            this.capturable_select.appendChild(option);
            if (name === current_selection)
                new_index = i;
        });
        if (new_index !== undefined)
            this.capturable_select.value = String(new_index);
        else if (current_selection)
            // Can't find the window, so don't select anything
            this.capturable_select.value = "";
        if (this.reselect_capture) {
            this.reselect_capture = false;
            // a new portal session renames its stream: take the first one so the video comes back
            if (!this.capturable_select.value && window_names.length > 0)
                this.capturable_select.value = "0";
            if (this.capturable_select.value)
                this.send_server_config();
        }
    }

    toggle_energysaving(energysaving: boolean) {
        let canvas = fresh_canvas();
        if (energysaving) {
            let ctx = canvas.getContext("2d");
            ctx.fillStyle = "#000";
            ctx.fillRect(0, 0, canvas.width, canvas.height);
        }

        if (energysaving) {
            this.checks.get("enable_video").checked = false;
            this.checks.get("enable_video").disabled = true;
            this.checks.get("enable_video").dispatchEvent(new Event("change"));
        } else
            this.checks.get("enable_video").disabled = false;
        if (settings)
            new PointerHandler(this.webSocket);
    }

    video_enabled(): boolean {
        return this.checks.get("enable_video").checked;
    }
}

let settings: Settings;
let debug_overlay: HTMLElement;
let last_pointer_data: Object;

class PEvent {
    event_type: string;
    pointer_id: number;
    timestamp: number;
    is_primary: boolean;
    pointer_type: string;
    button: number;
    buttons: number;
    x: number;
    y: number;
    // movement_x: number;
    // movement_y: number;
    pressure: number;
    tilt_x: number;
    tilt_y: number;
    twist: number;
    width: number;
    height: number;

    constructor(eventType: string, event: PointerEvent, targetRect: DOMRect) {
        let diag_len = Math.sqrt(targetRect.width * targetRect.width + targetRect.height * targetRect.height)
        this.event_type = eventType.toString();
        this.pointer_id = event.pointerId;
        this.timestamp = Math.round(event.timeStamp * 1000);
        this.is_primary = event.isPrimary;
        this.pointer_type = event.pointerType;
        let btn = event.button;
        // for some reason the secondary and auxiliary buttons are ordered differently for
        // the button and buttons properties
        if (btn == 2)
            btn = 1;
        else if (btn == 1)
            btn = 2;
        this.button = (btn < 0 ? 0 : 1 << btn);
        this.buttons = event.buttons;
        let x_offset = 0;
        let y_offset = 0;
        let x_scale = 1;
        let y_scale = 1;
        if (settings.checks.get("enable_custom_input_areas").checked) {
            let custom_input_area: Rect = null;
            if (event.pointerType == "mouse") {
                custom_input_area = settings.custom_input_areas.mouse;
            } else if (event.pointerType == "touch") {
                custom_input_area = settings.custom_input_areas.touch;
            } else if (event.pointerType == "pen") {
                custom_input_area = settings.custom_input_areas.pen;
            }
            if (custom_input_area) {
                x_scale = custom_input_area.w;
                y_scale = custom_input_area.h;
                x_offset = custom_input_area.x;
                y_offset = custom_input_area.y;
            }
        }
        this.x = (event.clientX - targetRect.left) / targetRect.width * x_scale + x_offset;
        this.y = (event.clientY - targetRect.top) / targetRect.height * y_scale + y_offset;
        // this.movement_x = event.movementX ? event.movementX : 0;
        // this.movement_y = event.movementY ? event.movementY : 0;
        let pressure_setting = settings.range_min_pressure.valueAsNumber;
        // pencils without a force sensor report a constant; "fixed" replaces it while touching
        if (event.pointerType == "pen" && settings.pen_pressure_mode.value == "fixed")
            this.pressure = (event.buttons & 1) || event.pressure > 0 ? pressure_setting : 0;
        else
            this.pressure = Math.max(event.pressure, pressure_setting);
        this.tilt_x = event.tiltX;
        this.tilt_y = event.tiltY;
        this.width = event.width / diag_len;
        this.height = event.height / diag_len;
        this.twist = event.twist;
    }
}

class WEvent {
    dx: number;
    dy: number;
    timestamp: number;

    constructor(event: WheelEvent) {
        /* The WheelEvent can have different scrolling modes that affect how much scrolling
         * should be done. Unfortunately there is not always a way to accurately convert the scroll
         * distance into pixels. Thus the following is a guesstimate and scales the WheelEvent's
         * deltaX/Y values accordingly.
         */
        let scale = 1;
        switch (event.deltaMode) {
            case 0x01: // DOM_DELTA_LINE
                scale = 10;
                break;
            case 0x02: // DOM_DELTA_PAGE
                scale = 1000;
                break;
            default: // DOM_DELTA_PIXEL
        }
        this.dx = Math.round(scale * event.deltaX);
        this.dy = Math.round(scale * event.deltaY);
        this.timestamp = Math.round(event.timeStamp * 1000);
    }
}

// in milliseconds
const fade_time = 5000;

const vs_source = `
  attribute vec3 aVertex;
  uniform float uTime;
  varying lowp vec4 vColor;

  void main() {
    float dt = uTime - aVertex[2];
    gl_Position = vec4(aVertex[0], aVertex[1], 1.0, 1.0);
    vColor = vec4(0.0, 170.0/255.0, 1.0, 1.0) * max(1.0 - dt/${fade_time}.0, 0.0);
  }
`;

const fs_source = `
  varying lowp vec4 vColor;

  void main() {
    gl_FragColor = vColor;
  }
`;

class Painter {
    canvas: HTMLCanvasElement;
    gl: WebGLRenderingContext;

    /* Store lines currently being drawn.
     *
     * Keys are pointerIds, values are an array of the last position (x, y), thickness and event
     * time and another array with vertices to be used by webgl. Each vertex is made of 3 floats, x
     * and y coordinates and the event time. Vertices always come in pairs of two. Two such vertices
     * describe the edges of the line to be drawn with regard to it's thickness. TRIANGLE_STRIP is
     * then used to connect them and draw an actual line with some thickness depending on the
     * pressure applied.
     */
    lines_active: Map<number, [[number, number, number, number], number[]]>

    // Array of vertices that are not actively drawn anymore and do not need updates, except
    // removing them after they faded away.
    lines_old: number[][];
    vertex_attr: GLint;
    vertex_buffer: WebGLBuffer;
    time_attr: WebGLUniformLocation;
    initialized: boolean;

    constructor(canvas: HTMLCanvasElement) {
        this.canvas = canvas;
        canvas.width = window.innerWidth * window.devicePixelRatio;
        canvas.height = window.innerHeight * window.devicePixelRatio;
        this.gl = canvas.getContext("webgl");
        if (this.gl) {
            this.lines_active = new Map();
            this.lines_old = [];
            this.setupWebGL();
        }
    }

    loadShader(type, source): WebGLShader {
        let gl = this.gl;
        const shader = gl.createShader(type);
        gl.shaderSource(shader, source);
        gl.compileShader(shader);
        if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
            log(LogLevel.WARN, "Failed to compile shaders: " + gl.getShaderInfoLog(shader));
            gl.deleteShader(shader);
            return null;
        }
        return shader;
    }

    setupWebGL() {
        let gl = this.gl;
        gl.enable(gl.BLEND);
        gl.clearColor(0, 0, 0, 0);
        gl.clear(gl.COLOR_BUFFER_BIT);

        const vertex_shader = this.loadShader(gl.VERTEX_SHADER, vs_source);
        const fragment_shader = this.loadShader(gl.FRAGMENT_SHADER, fs_source);
        if (!vertex_shader || !fragment_shader)
            return;
        const shader_program = gl.createProgram();
        gl.attachShader(shader_program, vertex_shader);
        gl.attachShader(shader_program, fragment_shader);
        gl.linkProgram(shader_program);

        if (!gl.getProgramParameter(shader_program, gl.LINK_STATUS)) {
            log(LogLevel.WARN, "Unable to initialize the shader program: " + gl.getProgramInfoLog(shader_program));
            return;
        }
        this.vertex_attr = gl.getAttribLocation(shader_program, "aVertex");
        this.time_attr = gl.getUniformLocation(shader_program, "uTime");
        this.vertex_buffer = gl.createBuffer();
        gl.bindBuffer(gl.ARRAY_BUFFER, this.vertex_buffer);
        gl.vertexAttribPointer(this.vertex_attr, 3, gl.FLOAT, false, 0, 0);
        gl.enableVertexAttribArray(this.vertex_attr);
        gl.useProgram(shader_program);
        this.initialized = true;
        requestAnimationFrame(() => this.render());
    }

    render() {
        // only do work if necessary
        if (!check_video.checked && (this.lines_active.size > 0 || this.lines_old.length > 0)) {
            if (this.lines_old.length > 0) {
                if (performance.now() - this.lines_old[0][this.lines_old[0].length - 1] > fade_time)
                    this.lines_old.shift();
            }
            let gl = this.gl;
            gl.viewport(0, 0, this.canvas.width, this.canvas.height);
            gl.clear(gl.COLOR_BUFFER_BIT);
            gl.uniform1f(this.time_attr, performance.now());
            gl.bindBuffer(gl.ARRAY_BUFFER, this.vertex_buffer);
            for (let vertices of this.lines_old) {
                gl.bufferData(gl.ARRAY_BUFFER, new Float32Array(vertices), gl.DYNAMIC_DRAW);
                gl.drawArrays(gl.TRIANGLE_STRIP, 0, vertices.length / 3)
            }
            for (let [_, vertices] of this.lines_active.values()) {
                // sometimes there are no linesegments because there has been only a single
                // PointerEvent
                if (vertices.length == 0)
                    continue;
                gl.bufferData(gl.ARRAY_BUFFER, new Float32Array(vertices), gl.DYNAMIC_DRAW);
                gl.drawArrays(gl.TRIANGLE_STRIP, 0, vertices.length / 3)
            }
        }
        requestAnimationFrame(() => this.render());
    }

    appendEventToLine(event: PointerEvent) {
        let line = this.lines_active.get(event.pointerId);
        if (!line) {
            line = [null, []];
            this.lines_active.set(event.pointerId, line)
        }
        let max_pixels = Math.max(this.canvas.width, this.canvas.height);
        let x = event.clientX * window.devicePixelRatio / this.canvas.width * 2 - 1;
        let y = 1 - event.clientY * window.devicePixelRatio / this.canvas.height * 2;
        let delta = event.pressure + 0.4;
        let t = performance.now();
        // to draw a line segment, there has to be some previous position
        if (line[0]) {
            let [x0, y0, delta0, t0] = line[0];
            // get vector perpendicular to the linesegment to calculate quadrangel around the
            // segment with appropriate thickness
            let dx = (y - y0);
            let dy = -(x - x0);
            let dd = Math.sqrt(dx ** 2 + dy ** 2);
            if (dd == 0) {
                return;
            }
            dx = dx / dd * max_pixels / this.canvas.width * 0.004;
            dy = dy / dd * max_pixels / this.canvas.height * 0.004;

            if (line[1].length == 0)
                line[1].push(
                    x0 + delta0 * dx, y0 + delta0 * dy, t0, x0 - delta0 * dx, y0 - delta0 * dy, t0,
                );
            line[1].push(
                x + delta * dx, y + delta * dy, t, x - delta * dx, y - delta * dy, t
            )
        }
        line[0] = [x, y, delta, t];
    }

    onstart(event: PointerEvent) {
        this.appendEventToLine(event);
    }

    onmove(event: PointerEvent) {
        if (this.lines_active.has(event.pointerId)) {
            const events = typeof event.getCoalescedEvents === 'function' ? event.getCoalescedEvents() : [event];
            for (const e of events) {
                this.appendEventToLine(e);
            }
        }
    }

    onstop(event: PointerEvent) {
        let lines = this.lines_active.get(event.pointerId);
        if (lines) {
            if (lines[1].length > 0)
                this.lines_old.push(lines[1]);
            this.lines_active.delete(event.pointerId);
        }
    }
}

class PointerHandler {
    webSocket: WebSocket;
    pointerTypes: string[];

    constructor(webSocket: WebSocket) {
        let video = document.getElementById("video");
        let canvas = document.getElementById("canvas");
        this.webSocket = webSocket;
        this.pointerTypes = settings.pointer_types();

        video.onpointerdown = (e) => this.onEvent(e, "pointerdown");
        video.onpointerup = (e) => this.onEvent(e, "pointerup");
        video.onpointercancel = (e) => this.onEvent(e, "pointercancel");
        video.onpointermove = (e) => this.onEvent(e, "pointermove");
        video.onpointerout = (e) => this.onEvent(e, "pointerout");
        video.onpointerleave = (e) => this.onEvent(e, "pointerleave");
        video.onpointerenter = (e) => this.onEvent(e, "pointerenter");
        video.onpointerover = (e) => this.onEvent(e, "pointerover");

        let painter: Painter;
        if (!settings.checks.get("energysaving").checked)
            painter = new Painter(canvas as HTMLCanvasElement);

        if (painter && painter.initialized) {
            canvas.onpointerdown = (e) => { this.onEvent(e, "pointerdown"); painter.onstart(e); };
            canvas.onpointerup = (e) => { this.onEvent(e, "pointerup"); painter.onstop(e); };
            canvas.onpointercancel = (e) => { this.onEvent(e, "pointercancel"); painter.onstop(e); };
            canvas.onpointermove = (e) => { this.onEvent(e, "pointermove"); painter.onmove(e); };
            canvas.onpointerout = (e) => { this.onEvent(e, "pointerout"); painter.onstop(e); };
            canvas.onpointerleave = (e) => { this.onEvent(e, "pointerleave"); painter.onstop(e); };
            canvas.onpointerenter = (e) => { this.onEvent(e, "pointerenter"); painter.onmove(e); };
            canvas.onpointerover = (e) => { this.onEvent(e, "pointerover"); painter.onmove(e); };
        } else {
            canvas.onpointerdown = (e) => this.onEvent(e, "pointerdown");
            canvas.onpointerup = (e) => this.onEvent(e, "pointerup");
            canvas.onpointercancel = (e) => this.onEvent(e, "pointercancel");
            canvas.onpointermove = (e) => this.onEvent(e, "pointermove");
            canvas.onpointerout = (e) => this.onEvent(e, "pointerout");
            canvas.onpointerleave = (e) => this.onEvent(e, "pointerleave");
            canvas.onpointerenter = (e) => this.onEvent(e, "pointerenter");
            canvas.onpointerover = (e) => this.onEvent(e, "pointerover");
        }

        // This is a workaround for the following Safari/WebKit bug:
        // https://bugs.webkit.org/show_bug.cgi?id=217430
        // I have no idea why this works but it does.
        video.ontouchmove = (e) => e.preventDefault();
        canvas.ontouchmove = (e) => e.preventDefault();

        for (let elem of [video, canvas]) {
            elem.onwheel = (e) => {
                this.webSocket.send(JSON.stringify({ "WheelEvent": new WEvent(e) }));
            }
        }
    }

    onEvent(event: PointerEvent, event_type: string) {
        if (settings.checks.get("enable_debug_overlay").checked) {
            let props = [
                "altKey",
                "altitudeAngle",
                "azimuthAngle",
                "button",
                "buttons",
                "clientX",
                "clientY",
                "ctrlKey",
                "height",
                "isPrimary",
                "metaKey",
                "movementX",
                "movementY",
                "offsetX",
                "offsetY",
                "pageX",
                "pageY",
                "pointerId",
                "pointerType",
                "pressure",
                "screenX",
                "screenY",
                "shiftKey",
                "tangentialPressure",
                "tiltX",
                "tiltY",
                "timeStamp",
                "twist",
                "type",
                "width",
                "x",
                "y",
            ];
            if (!last_pointer_data) {
                last_pointer_data = {};
                for (let prop of props) {
                    let span_id = `prop_${prop}_span`;
                    let span = document.getElementById(span_id);
                    span = document.createElement("span");
                    span.id = span_id;
                    debug_overlay.appendChild(span);
                    debug_overlay.appendChild(document.createElement("br"));
                }
            }
            for (let prop of props) {
                let span_id = `prop_${prop}_span`;
                let span = document.getElementById(span_id);
                let v = event[prop];
                span.textContent = `${prop}: ${v}`;
                if (last_pointer_data[prop] == v) {
                    span.classList.remove("updated");
                } else {
                    span.classList.add("updated");
                    last_pointer_data[prop] = v;
                }
            }
        }
        if (this.pointerTypes.includes(event.pointerType)) {
            let rect = (event.target as HTMLElement).getBoundingClientRect();
            const events = event_type === "pointermove" && typeof event.getCoalescedEvents === 'function' ? event.getCoalescedEvents() : [event];
            for (let event of events) {
                this.webSocket.send(
                    JSON.stringify(
                        {
                            "PointerEvent": new PEvent(
                                event_type,
                                event,
                                rect
                            )
                        }
                    )
                );
            }
            if (settings.visible) {
                settings.toggle();
            }
        }
    }
}

class KEvent {
    event_type: string;
    code: string;
    key: string;
    location: number;
    alt: boolean;
    ctrl: boolean;
    shift: boolean;
    meta: boolean;

    constructor(event_type: string, event: KeyboardEvent) {
        this.event_type = event_type;
        this.code = event.code;
        this.key = event.key;
        this.location = event.location;
        this.alt = event.altKey;
        this.ctrl = event.ctrlKey;
        this.shift = event.shiftKey;
        this.meta = event.metaKey;
    }
}

class KeyboardHandler {
    webSocket: WebSocket;

    constructor(webSocket: WebSocket) {
        this.webSocket = webSocket;

        let d = document;
        let s = document.getElementById("settings")

        // Consume all KeyboardEvents, except the settings menu is open.
        // this avoids making the main/video/canvas element focusable by using
        // a tabindex which interferes with PointerEvent than can be considered
        // hovering.

        function settings_hidden() {
            return s.classList.contains("hide") || s.classList.contains("vanish");
        }

        d.onkeydown = (e) => {
            if (!settings_hidden())
                return true;
            if (e.repeat)
                return this.onEvent(e, "repeat");
            return this.onEvent(e, "down");
        };
        d.onkeyup = (e) => {
            if (!settings_hidden())
                return true;
            return this.onEvent(e, "up");
        };
        d.onkeypress = (e) => {
            if (!settings_hidden())
                return true;
            e.preventDefault();
            e.stopPropagation();
            return false;
        };
    }

    onEvent(event: KeyboardEvent, event_type: string) {
        this.webSocket.send(JSON.stringify({ "KeyboardEvent": new KEvent(event_type, event) }));
        event.preventDefault();
        event.stopPropagation();
        return false;
    }
}

function frame_rate_stats() {
    let t = performance.now();
    let elapsed_s = (t - last_fps_calc) / 1000;
    let fps = Math.round(frame_count / elapsed_s * 10) / 10;
    fps_out.value = fps.toString();
    if (settings?.checks.get("enable_stats_overlay")?.checked) {
        let vs = video_stats;
        let set = (id: string, v: string | null) => {
            let el = document.getElementById(id) as HTMLOutputElement | null;
            if (el)
                el.value = v ?? "\u2014";
        };
        let avg = (a: number[]) => a.reduce((x, y) => x + y, 0) / a.length;
        let shown = Math.round(vs.presented / elapsed_s * 10) / 10;
        set("stats_fps", fps + " / " + shown);
        set("stats_decode", vs.decode_ms.length ? avg(vs.decode_ms).toFixed(1) + " ms" : null);
        set("stats_latency", vs.latency_ms.length
            ? "avg " + Math.round(avg(vs.latency_ms)) + " ms, max " + Math.round(Math.max(...vs.latency_ms)) + " ms"
            : null);
        set("stats_rtt", vs.rtt != null ? Math.round(vs.rtt) + " ms" : null);
        set("stats_dropped", vs.dropped + " (total " + (vs.dropped_total + vs.dropped) + ")");
        set("stats_bitrate", (vs.bytes * 8 / elapsed_s / 1e6).toFixed(1) + " Mbps");
        set("stats_codec", vs.codec || null);
        set("stats_resolution", vs.width > 0 ? vs.width + "\u00d7" + vs.height : null);
        vs.ping(log_socket);
    }
    video_stats.dropped_total += video_stats.dropped;
    video_stats.dropped = 0;
    video_stats.bytes = 0;
    video_stats.presented = 0;
    video_stats.decode_ms = [];
    video_stats.latency_ms = [];
    frame_count = 0;
    last_fps_calc = t;
    setTimeout(() => frame_rate_stats(), 1000);
}

function handle_messages(
    webSocket: WebSocket,
    video: HTMLCanvasElement,
    onConfigOk: Function,
    onConfigError: Function,
    onCapturableList: Function,
) {
    const ctx2d = video.getContext("2d", { alpha: false, desynchronized: true });
    let decoder: VideoDecoder | null = null;
    let config: VideoDecoderConfig | null = null;
    let need_key = true;
    let pending: VideoFrame | null = null;
    let raf_scheduled = false;
    let last_key_request = 0;
    let version_error_logged = false;
    // codec fallback bookkeeping (see Settings.try_codec_fallback)
    let active_codec = "";
    let fail_times: number[] = [];
    let config_time = 0;
    let got_frame = false;

    // an explicit non-h264 codec that failed twice within 5 s is dropped in favour of Auto
    function codec_failure(reason: string) {
        let t = performance.now();
        fail_times = fail_times.filter((x) => t - x < 5000);
        fail_times.push(t);
        if (fail_times.length >= 2 && settings.try_codec_fallback(active_codec, reason))
            fail_times = [];
    }

    // decoder.configure may throw synchronously (e.g. NotSupportedError)
    function configure(d: VideoDecoder, c: VideoDecoderConfig): boolean {
        try {
            d.configure(c);
            return true;
        } catch (e) {
            log(LogLevel.WARN, "Failed to configure video decoder: " + e);
            codec_failure("configure threw: " + e);
            return false;
        }
    }

    // rate limited so a congested link isn't flooded with requests
    function request_keyframe() {
        let t = performance.now();
        if (t - last_key_request > 300) {
            last_key_request = t;
            webSocket.send('"RequestKeyframe"');
        }
    }

    function draw() {
        raf_scheduled = false;
        let frame = pending;
        pending = null;
        if (!frame)
            return;
        if (video.width != frame.displayWidth || video.height != frame.displayHeight) {
            video.width = frame.displayWidth;
            video.height = frame.displayHeight;
            stretch_video();
        }
        ctx2d.drawImage(frame, 0, 0);
        video_stats.on_present(frame);
        frame.close();
    }

    function on_frame(frame: VideoFrame) {
        video_stats.on_decoded(frame.timestamp);
        got_frame = true;
        // render-side drop: only the newest frame per display refresh is drawn
        if (pending)
            video_stats.dropped++;
        pending?.close();
        pending = frame;
        if (!raf_scheduled) {
            raf_scheduled = true;
            requestAnimationFrame(draw);
        }
    }

    function new_decoder() {
        if (decoder && decoder.state != "closed") {
            try {
                decoder.close();
            } catch (e) { }
        }
        let d: VideoDecoder = new VideoDecoder({
            output: on_frame,
            error: (e) => {
                // a closed or replaced decoder may still report (e.g. AbortError): it must not null the current one
                if (decoder !== d)
                    return;
                log(LogLevel.WARN, "Video decoder error: " + e.name + ": " + e.message);
                codec_failure("decoder error " + e.name);
                need_key = true;
                decoder = null;
                request_keyframe();
            },
        });
        decoder = d;
        if (config)
            configure(d, config);
    }

    function handle_video_message(buf: ArrayBuffer) {
        let view = new DataView(buf);
        if (view.getUint8(0) != 2) {
            if (!version_error_logged) {
                version_error_logged = true;
                log(LogLevel.ERROR, "Video format version mismatch between server and client, reload the page.");
            }
            return;
        }
        let flags = view.getUint8(1);
        let codec_id = view.getUint8(2);
        let pts_ms = view.getUint32(4, true);
        settings.show_active_codec(codec_id);
        let codec_name = CODEC_IDS[codec_id] ?? "";
        video_stats.codec = CODEC_LABELS[codec_name] ?? codec_name;
        if (codec_name != active_codec) {
            active_codec = codec_name;
            fail_times = [];
        }
        // chunks keep arriving but nothing was ever decoded for the current config
        if (!(flags & 2) && config_time > 0 && !got_frame && performance.now() - config_time > 3000) {
            config_time = 0;
            settings.try_codec_fallback(active_codec, "no frame decoded 3 s after the codec config");
        }
        let off = 8;
        if (flags & 2) {
            let s = view.getUint8(8);
            let codec = String.fromCharCode(...new Uint8Array(buf, 9, s));
            let n = view.getUint32(9 + s, true);
            off = 13 + s + n;
            let first = config == null;
            let new_config: VideoDecoderConfig = {
                codec: codec,
                optimizeForLatency: true,
                hardwareAcceleration: "prefer-hardware",
            };
            if (n > 0)
                new_config.description = new Uint8Array(buf, 13 + s, n).slice();
            config = new_config;
            log(LogLevel.INFO, "Video codec: " + codec);
            if (first) {
                let c = config;
                VideoDecoder.isConfigSupported(c).then((r) => {
                    if (!r.supported)
                        log(LogLevel.ERROR, codec + " not supported by this browser's VideoDecoder");
                }).catch(() => { });
            }
            // the received codec string itself may be unsupported: counts as one failure
            let received_codec = active_codec;
            VideoDecoder.isConfigSupported(config).then((r) => {
                if (!r.supported && received_codec == active_codec)
                    codec_failure("isConfigSupported false for " + codec);
            }).catch(() => { });
            got_frame = false;
            config_time = performance.now();
            if (!decoder || decoder.state == "closed")
                new_decoder();
            else
                configure(decoder, config);
            need_key = true;
        }
        let key = (flags & 1) != 0;
        // after a decoder error the config is only resent when SPS/PPS change: rebuild from the cached one
        if (key && config && (!decoder || decoder.state == "closed"))
            new_decoder();
        if (!decoder || !config || decoder.state != "configured") {
            if (!key)
                request_keyframe();
            video_stats.dropped++;
            return;
        }
        if (!key && (need_key || decoder.decodeQueueSize > 1)) {
            // a delta frame can't be dropped alone (later frames reference it): skip until the next keyframe
            need_key = true;
            request_keyframe();
            video_stats.dropped++;
            return;
        }
        if (key && decoder.decodeQueueSize > 1) {
            // the decoder is behind: a keyframe lets us flush the whole queue and resync
            video_stats.dropped += decoder.decodeQueueSize;
            decoder.reset();
            configure(decoder, config);
        }
        video_stats.on_submit(pts_ms * 1000);
        try {
            decoder.decode(new EncodedVideoChunk({
                type: key ? "key" : "delta",
                timestamp: pts_ms * 1000,
                data: new Uint8Array(buf, off),
            }));
        } catch (e) {
            log(LogLevel.WARN, "Failed to decode video frame: " + e);
            video_stats.dropped++;
            need_key = true;
            request_keyframe();
            return;
        }
        if (key)
            need_key = false;
    }

    webSocket.onmessage = (event: MessageEvent) => {
        if (typeof event.data == "string") {
            let msg = JSON.parse(event.data);
            if (typeof msg == "string") {
                if (msg == "NewVideo") {
                    need_key = true;
                    video_stats.epoch = null;
                    config = null;
                    config_time = 0;
                    new_decoder();
                } else if (msg == "ConfigOk") {
                    onConfigOk();
                }
            } else if (typeof msg == "object") {
                if ("CapturableList" in msg)
                    onCapturableList(msg["CapturableList"]);
                else if ("Error" in msg) {
                    settings?.reset_screen_pick();
                    alert(msg["Error"]);
                } else if ("ConfigError" in msg) {
                    settings?.reset_screen_pick();
                    onConfigError(msg["ConfigError"]);
                } else if ("CustomInputAreas" in msg) {
                    settings.custom_input_areas = msg["CustomInputAreas"];
                    settings.checks.get("enable_custom_input_areas").checked = true;
                    settings.save_settings();
                } else if ("Pong" in msg) {
                    video_stats.on_pong(msg["Pong"]);
                } else if ("VideoEpoch" in msg) {
                    video_stats.epoch = msg["VideoEpoch"];
                }
            }

            return;
        }

        // not a string -> got a video frame
        frame_count += 1;
        video_stats.bytes += (event.data as ArrayBuffer).byteLength;
        handle_video_message(event.data);
    }
}

function check_apis() {
    let apis = [
        {
            attrs: ["VideoDecoder"],
            msg: window.isSecureContext
                ? "This browser doesn't support WebCodecs (VideoDecoder) required to play the video stream, try upgrading (iPadOS 16.4+)!"
                : "Video decoding (WebCodecs) needs a secure page: turn on HTTPS in Weylus' settings and open the https:// address."
        },
        {
            attrs: ["PointerEvent"],
            msg: "This browser doesn't support PointerEvents, input will not work, try upgrading!"
        },
    ];

    outer:
    for (let d of apis) {
        for (let attr of d.attrs) {
            if (attr in window) {
                continue outer;
            }
        }
        log(LogLevel.ERROR, d.msg);
    }
}

function init() {
    check_apis();

    let protocol = document.location.protocol == "https:" ? "wss://" : "ws://";
    let webSocket = new WebSocket(
        protocol + window.location.hostname + ":" +
        window.location.port + "/ws" + window.location.search
    );
    webSocket.binaryType = "arraybuffer";
    log_socket = webSocket;

    debug_overlay = document.getElementById("debug_overlay");
    stats_overlay = document.getElementById("stats_overlay");
    settings = new Settings(webSocket);

    let video = document.getElementById("video") as HTMLCanvasElement;
    let canvas = document.getElementById("canvas") as HTMLCanvasElement;

    video.oncontextmenu = function(event) {
        event.preventDefault();
        event.stopPropagation();
        return false;
    };
    canvas.oncontextmenu = function(event) {
        event.preventDefault();
        event.stopPropagation();
        return false;
    };

    init_view_controls(video);
    shortcut_bar = new ShortcutBar(webSocket);

    let toggle_fullscreen_btn = document.getElementById("fullscreen") as HTMLButtonElement;

    if (document.exitFullscreen) {
        toggle_fullscreen_btn.onclick = () => {
            if (!document.fullscreenElement)
                document.body.requestFullscreen({ navigationUI: "hide" });
            else
                document.exitFullscreen();
        }
    } else {
        // if document.exitFullscreen is not present we are probably running on iOS/iPadOS.
        // As input is broken in fullscreen mode on these, do not offer fullscreen in the first
        // place.
        toggle_fullscreen_btn.parentElement.removeChild(toggle_fullscreen_btn);
    }

    let handle_disconnect = (msg: string) => {
        document.body.onclick = video.onclick = (e) => {
            e.stopPropagation();
            if (window.confirm(msg + " Reload page?"))
                location.reload();
        }
    }
    webSocket.onerror = () => handle_disconnect("Lost connection.");
    webSocket.onclose = () => handle_disconnect("Connection closed.");
    window.onresize = () => {
        stretch_video();
        canvas.width = window.innerWidth * window.devicePixelRatio;
        canvas.height = window.innerHeight * window.devicePixelRatio;
        let [w, h] = calc_max_video_resolution(settings.scale_video_input.valueAsNumber);
        settings.scale_video_output.value = w + "x" + h;
        settings.send_server_config();
    }
    let is_connected = false;
    handle_messages(webSocket, video, () => {
        if (!is_connected) {
            new KeyboardHandler(webSocket);
            new PointerHandler(webSocket);
            is_connected = true;
        }
    },
        (err) => alert(err),
        (window_names) => settings.onCapturableList(window_names)
    );
    window.onunload = () => { webSocket.close(); }
    webSocket.onopen = function(event) {
        webSocket.send(JSON.stringify({ "SetCaptureCursor": settings.checks.get("capture_cursor").checked }));
        webSocket.send('"GetCapturableList"');
        if (!settings.video_enabled())
            webSocket.send('"PauseVideo"');

        settings.codecs_probed.then(() => {
            log(LogLevel.INFO, "Decodable video codecs: " + [...settings.decodable].join(", "));
            settings.send_server_config();
        });

        document.onvisibilitychange = () => {
            if (document.hidden) {
                webSocket.send('"PauseVideo"');
            } else if (settings.video_enabled()) {
                webSocket.send('"ResumeVideo"');
            }
        };
    }
    frame_rate_stats();
}

// View zoom/pan of the video. The transform is applied to #video itself, so getBoundingClientRect()
// (used for the pointer coordinates in PointerHandler.onEvent) already includes it.
const VIEW_ZOOM_MAX = 8;
const VIEW_ZOOM_STEP = 1.25;
let view_zoom = 1;
let view_tx = 0;
let view_ty = 0;
// fit/stretch scale of the unzoomed view
let view_base_sx = 1;
let view_base_sy = 1;

// object-fit: fill; <-- this is unfortunately not supported on iOS, so we use the following
// workaround
function stretch_video() {
    let video = document.getElementById("video") as HTMLCanvasElement;
    if (!video.clientWidth || !video.clientHeight)
        return;
    if (settings.stretched_video()) {
        view_base_sx = document.body.clientWidth / video.clientWidth;
        view_base_sy = document.body.clientHeight / video.clientHeight;
    } else {
        view_base_sx = view_base_sy = Math.min(document.body.clientWidth / video.clientWidth, document.body.clientHeight / video.clientHeight);
    }
    apply_view_transform();
}

// pan range along one axis so the picture covers the viewport where it can, else it is centered;
// c is the (untransformed) layout centre of the video
function clamp_pan(t: number, size: number, viewport: number, c: number) {
    if (size <= viewport)
        return viewport / 2 - c;
    return Math.min(Math.max(t, viewport - c - size / 2), size / 2 - c);
}

// the only place that writes the transform of #video
function apply_view_transform() {
    let video = document.getElementById("video") as HTMLCanvasElement;
    if (view_zoom <= 1) {
        view_zoom = 1;
        view_tx = view_ty = 0;
    } else {
        let cx = video.offsetLeft + video.clientWidth / 2;
        let cy = video.offsetTop + video.clientHeight / 2;
        view_tx = clamp_pan(view_tx, video.clientWidth * view_base_sx * view_zoom, document.body.clientWidth, cx);
        view_ty = clamp_pan(view_ty, video.clientHeight * view_base_sy * view_zoom, document.body.clientHeight, cy);
    }
    video.style.transform = "translate(" + view_tx + "px, " + view_ty + "px) scale(" +
        view_base_sx * view_zoom + ", " + view_base_sy * view_zoom + ")";
    let out = document.getElementById("zoom_level") as HTMLOutputElement | null;
    if (out) {
        let level = Math.round(view_zoom * 100) + "%";
        if (out.value != level)
            wake_zoom_controls();
        out.value = level;
    }
}

const SHORTCUT_BAR_IDLE_MS = 4000;

let shortcut_bar: ShortcutBar | null = null;

// Edge toolbar that sends key combos to the PC. Every key is its own KeyboardEvent (modifier flags
// stay false), and held keys are ref-counted so a held Ctrl plus a tapped Undo never releases Ctrl early.
class ShortcutBar {
    ws: WebSocket;
    bar: HTMLElement;
    buttons: HTMLElement;
    icons: HTMLTemplateElement;
    held = new Map<string, number>();
    // hold buttons currently pressed: release function and the button
    pressed = new Set<{ release: () => void }>();
    timer: number | undefined;

    constructor(ws: WebSocket) {
        this.ws = ws;
        this.bar = document.getElementById("shortcut_bar");
        this.buttons = document.getElementById("shortcut_buttons");
        this.icons = document.getElementById("shortcut_icons") as HTMLTemplateElement;
        // the bar never reaches the PC
        for (let ev of ["click", "pointerdown", "pointerup", "pointermove", "pointercancel"])
            this.bar.addEventListener(ev, (e) => e.stopPropagation());
        this.bar.addEventListener("contextmenu", (e) => e.preventDefault());
        // Open from the grip on click, like the zoom controls.
        document.getElementById("shortcut_grip").onclick = () => this.wake();
        this.bar.addEventListener("pointerdown", () => {
            if (!this.bar.classList.contains("collapsed"))
                this.wake();
        });
        window.addEventListener("blur", () => this.release_all());
        document.addEventListener("visibilitychange", () => {
            if (document.hidden)
                this.release_all();
        });
        this.rebuild();
    }

    send(code: string, type: "down" | "up") {
        if (this.ws.readyState != WebSocket.OPEN)
            return;
        this.ws.send(JSON.stringify({
            "KeyboardEvent": {
                event_type: type, code, key: "", location: 0,
                alt: false, ctrl: false, shift: false, meta: false,
            }
        }));
    }

    press(codes: string[]) {
        for (let code of codes) {
            let n = this.held.get(code) ?? 0;
            this.held.set(code, n + 1);
            if (n == 0)
                this.send(code, "down");
        }
    }

    release(codes: string[]) {
        for (let code of [...codes].reverse()) {
            let n = this.held.get(code) ?? 0;
            if (n <= 0)
                continue;
            if (n == 1) {
                this.held.delete(code);
                this.send(code, "up");
            } else {
                this.held.set(code, n - 1);
            }
        }
    }

    release_all() {
        for (let p of [...this.pressed])
            p.release();
        this.pressed.clear();
        for (let code of [...this.held.keys()])
            this.send(code, "up");
        this.held.clear();
    }

    rebuild() {
        this.release_all();
        let enabled = settings.checks.get("enable_shortcuts").checked;
        let list: Shortcut[] = [];
        let preset = SHORTCUT_PRESETS[settings.shortcut_preset.value] ?? [];
        for (let [label, combo, hold, icon] of preset) {
            let codes = parse_combo(combo);
            if (codes)
                list.push({ label, codes, hold, icon });
        }
        let custom = parse_custom_shortcuts(settings.shortcut_custom.value);
        list.push(...custom.shortcuts);
        let status = document.getElementById("shortcut_custom_status") as HTMLOutputElement | null;
        if (status)
            status.value = custom.errors.join("\n");

        this.bar.classList.toggle("hide", !enabled || list.length == 0);
        this.bar.classList.toggle("right", settings.shortcut_side.value == "right");

        this.buttons.textContent = "";
        for (let sc of list)
            this.buttons.appendChild(this.make_button(sc));
        this.wake();
    }

    make_button(sc: Shortcut): HTMLButtonElement {
        let btn = document.createElement("button");
        btn.type = "button";
        btn.className = sc.hold ? "shortcut hold" : "shortcut";
        btn.title = sc.label;
        btn.setAttribute("aria-label", sc.label);
        let svg = sc.icon ? this.icons?.content.querySelector('svg[data-icon="' + sc.icon + '"]') : null;
        if (svg) {
            btn.appendChild(svg.cloneNode(true));
        } else {
            let span = document.createElement("span");
            span.className = "shortcut_label";
            span.textContent = sc.label;
            btn.appendChild(span);
        }
        let tap = () => {
            this.press(sc.codes);
            this.release(sc.codes);
        };
        if (!sc.hold) {
            btn.onclick = tap;
            return btn;
        }
        let state = {
            is_pressed: false,
            release: () => {
                if (!state.is_pressed)
                    return;
                state.is_pressed = false;
                this.pressed.delete(state);
                this.release(sc.codes);
                btn.classList.remove("active");
            },
        } as { is_pressed: boolean, release: () => void };
        btn.onpointerdown = (e) => {
            e.preventDefault();
            try { btn.setPointerCapture(e.pointerId); } catch { }
            if (state.is_pressed)
                return;
            state.is_pressed = true;
            this.pressed.add(state);
            this.press(sc.codes);
            btn.classList.add("active");
        };
        let up = () => {
            if (state.is_pressed) {
                state.release();
                this.wake();
            }
        };
        btn.onpointerup = up;
        btn.onpointercancel = up;
        btn.onlostpointercapture = up;
        // keyboard activation (detail == 0) is a tap; a real pointer click already did its work above
        btn.onclick = (e) => {
            if (e.detail == 0)
                tap();
        };
        return btn;
    }

    wake() {
        this.bar.classList.remove("collapsed");
        window.clearTimeout(this.timer);
        this.timer = window.setTimeout(() => this.collapse(), SHORTCUT_BAR_IDLE_MS);
    }

    collapse() {
        if (this.held.size > 0) {
            this.timer = window.setTimeout(() => this.collapse(), SHORTCUT_BAR_IDLE_MS);
            return;
        }
        this.bar.classList.add("collapsed");
    }
}

const ZOOM_CONTROLS_IDLE_MS = 3000;
let zoom_controls_timer: number | undefined;

// Shows the zoom controls and collapses them to the grip after a while without use.
function wake_zoom_controls() {
    let controls = document.getElementById("zoom_controls");
    if (!controls)
        return;
    controls.classList.remove("collapsed");
    window.clearTimeout(zoom_controls_timer);
    zoom_controls_timer = window.setTimeout(() => controls.classList.add("collapsed"), ZOOM_CONTROLS_IDLE_MS);
}

// Scale the view by r while the content point under (ox, oy) moves to (nx, ny) (client coordinates).
// r = 1 is a pure pan, ox == nx && oy == ny a pure zoom.
function zoom_view(r: number, ox: number, oy: number, nx: number, ny: number) {
    let video = document.getElementById("video") as HTMLCanvasElement;
    let cx = video.offsetLeft + video.clientWidth / 2;
    let cy = video.offsetTop + video.clientHeight / 2;
    let z = Math.min(Math.max(view_zoom * r, 1), VIEW_ZOOM_MAX);
    r = z / view_zoom;
    view_tx = (nx - cx) - r * ((ox - cx) - view_tx);
    view_ty = (ny - cy) - r * ((oy - cy) - view_ty);
    view_zoom = z;
    apply_view_transform();
}

function reset_view() {
    view_zoom = 1;
    apply_view_transform();
}

function init_view_controls(video: HTMLElement) {
    let controls = document.getElementById("zoom_controls");
    if (controls) {
        // the controls never reach the PC
        for (let ev of ["click", "pointerdown", "pointerup", "pointermove", "pointercancel"])
            controls.addEventListener(ev, (e) => e.stopPropagation());
        let centre = () => [document.body.clientWidth / 2, document.body.clientHeight / 2];
        let step = (r: number) => {
            let [x, y] = centre();
            zoom_view(r, x, y, x, y);
        };
        document.getElementById("zoom_in").onclick = () => step(VIEW_ZOOM_STEP);
        document.getElementById("zoom_out").onclick = () => step(1 / VIEW_ZOOM_STEP);
        document.getElementById("zoom_reset").onclick = () => reset_view();
        // Open from the grip on click, not pointerdown: expanding on pointerdown would move a zoom
        // button under the finger and the click would land on it.
        document.getElementById("zoom_grip").onclick = () => wake_zoom_controls();
        controls.addEventListener("pointerdown", () => {
            if (!controls.classList.contains("collapsed"))
                wake_zoom_controls();
        });
        wake_zoom_controls();
    }
    apply_view_transform();

    // Finger gestures only while touch input to the PC is off: two fingers pinch-zoom and pan,
    // one finger pans a zoomed view. Pen and mouse are ignored here, so they keep working.
    let touches = new Map<number, [number, number]>();
    let prev: { x: number, y: number, d: number } | null = null;
    let state = () => {
        let pts = [...touches.values()];
        if (pts.length == 0)
            return null;
        if (pts.length == 1)
            return { x: pts[0][0], y: pts[0][1], d: 0 };
        let [a, b] = pts;
        return { x: (a[0] + b[0]) / 2, y: (a[1] + b[1]) / 2, d: Math.hypot(a[0] - b[0], a[1] - b[1]) };
    };
    let relevant = (e: PointerEvent) => {
        if (e.pointerType != "touch")
            return false;
        if (settings.checks.get("enable_touch").checked) {
            // touch input is on: fingers behave as usual
            touches.clear();
            prev = null;
            return false;
        }
        return true;
    };
    video.addEventListener("pointerdown", (e) => {
        if (!relevant(e))
            return;
        touches.set(e.pointerId, [e.clientX, e.clientY]);
        prev = state();
    });
    video.addEventListener("pointermove", (e) => {
        if (!relevant(e) || !touches.has(e.pointerId))
            return;
        touches.set(e.pointerId, [e.clientX, e.clientY]);
        let cur = state();
        if (prev && cur) {
            // a one-finger state has d == 0: no zoom then
            let r = prev.d > 1 && cur.d > 1 ? cur.d / prev.d : 1;
            if (r != 1 || view_zoom > 1)
                zoom_view(r, prev.x, prev.y, cur.x, cur.y);
        }
        prev = cur;
    });
    let end = (e: PointerEvent) => {
        if (e.pointerType != "touch" || !touches.delete(e.pointerId))
            return;
        // a remaining finger continues as pan
        prev = state();
    };
    video.addEventListener("pointerup", end);
    video.addEventListener("pointercancel", end);
    // iOS would otherwise zoom the page itself
    for (let ev of ["gesturestart", "gesturechange", "gestureend"])
        video.addEventListener(ev, (e) => e.preventDefault(), { passive: false });
}
