interface Settings {
    access_code: string;
    allowed_clients: string;
    ask_new_devices: boolean;
    bind_address: string;
    web_port: number;
    auto_start: boolean;
    wayland_support: boolean;
    remember_screen: boolean;
    close_to_tray: boolean;
    try_vaapi: boolean;
    try_nvenc: boolean;
    check_updates: boolean;
}

interface ServerState {
    running: boolean;
    url: string | null;
    qr_svg: string | null;
    other_urls: string[];
}

type UpdateStateName =
    | "idle"
    | "checking"
    | "up_to_date"
    | "available"
    | "downloading"
    | "installed"
    | "check_failed"
    | "update_failed";

interface UpdateView {
    state: UpdateStateName;
    version: string | null;
    error: string | null;
    fraction: number | null;
    progress: string | null;
    current_version: string;
    install_kind: string;
    blocker: string | null;
}

interface LogLine {
    seq: number;
    line: string;
}

/** Fractions of the screen, 0..1. */
interface Rect {
    x: number;
    y: number;
    w: number;
    h: number;
}

interface CustomInputAreas {
    mouse: Rect | null;
    touch: Rect | null;
    pen: Rect | null;
}

interface VideoInfo {
    encoder: string;
    width: number;
    height: number;
    fps: number;
}

interface ClientInfo {
    id: number;
    ip: string;
    /** Unix time in milliseconds. */
    connected_at_ms: number;
    video: VideoInfo | null;
}

interface ApprovalPrompt {
    id: number;
    ip: string;
    timeout_secs: number;
}

type Approval = "once" | "always" | "deny";

interface Window {
    __TAURI__: {
        core: {
            invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T>;
        };
        event: {
            listen<T>(
                name: string,
                cb: (e: { payload: T }) => void
            ): Promise<() => void>;
        };
    };
}
