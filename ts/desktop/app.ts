const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const MAX_LOG_NODES = 1000;
const DEVICES = ["mouse", "touch", "pen"] as const;
type Device = (typeof DEVICES)[number];

function $<T extends HTMLElement = HTMLElement>(id: string): T {
    return document.getElementById(id) as T;
}

let running = false;
let pending = false;
let update: UpdateView | null = null;
let lastLogSeq = -1;

function errorText(err: unknown): string {
    return typeof err === "string" ? err : String(err);
}

function readSettings(): Settings {
    return {
        access_code: $<HTMLInputElement>("access_code").value,
        allowed_clients: $<HTMLInputElement>("allowed_clients").value,
        ask_new_devices: $<HTMLInputElement>("ask_new_devices").checked,
        bind_address: $<HTMLInputElement>("bind_address").value,
        web_port: parseInt($<HTMLInputElement>("web_port").value, 10),
        auto_start: $<HTMLInputElement>("auto_start").checked,
        wayland_support: $<HTMLInputElement>("wayland_support").checked,
        remember_screen: $<HTMLInputElement>("remember_screen").checked,
        close_to_tray: $<HTMLInputElement>("close_to_tray").checked,
        try_vaapi: $<HTMLInputElement>("try_vaapi").checked,
        try_nvenc: $<HTMLInputElement>("try_nvenc").checked,
        check_updates: $<HTMLInputElement>("check_updates").checked,
    };
}

function showSettings(s: Settings) {
    $<HTMLInputElement>("access_code").value = s.access_code;
    $<HTMLInputElement>("allowed_clients").value = s.allowed_clients;
    $<HTMLInputElement>("ask_new_devices").checked = s.ask_new_devices;
    $<HTMLInputElement>("bind_address").value = s.bind_address;
    $<HTMLInputElement>("web_port").value = String(s.web_port);
    $<HTMLInputElement>("auto_start").checked = s.auto_start;
    $<HTMLInputElement>("wayland_support").checked = s.wayland_support;
    $<HTMLInputElement>("remember_screen").checked = s.remember_screen;
    $<HTMLInputElement>("close_to_tray").checked = s.close_to_tray;
    $<HTMLInputElement>("try_vaapi").checked = s.try_vaapi;
    $<HTMLInputElement>("try_nvenc").checked = s.try_nvenc;
    $<HTMLInputElement>("check_updates").checked = s.check_updates;
}

function renderServer(state: ServerState) {
    running = state.running;
    const toggle = $<HTMLButtonElement>("toggle_server");
    toggle.dataset.running = String(running);
    $("toggle_server_label").textContent = running ? "Stop" : "Start";
    $("connect").hidden = !running;
    $("clients").hidden = !running;
    $("connect_url").textContent = state.url ?? "";
    $("qr").innerHTML = state.qr_svg ?? "";
    const others = $("other_urls");
    others.replaceChildren(
        ...state.other_urls.map((url) => {
            const li = document.createElement("li");
            li.textContent = url;
            return li;
        })
    );
    renderBusy();
}

function renderBusy() {
    $<HTMLFieldSetElement>("settings").disabled = running || pending;
    $<HTMLButtonElement>("toggle_server").disabled = pending;
}

function showSettingsError(message: string | null) {
    const el = $("settings_error");
    el.textContent = message ?? "";
    el.hidden = message === null;
    // The banner sits at the end of the settings card, usually below the fold.
    if (message !== null) el.scrollIntoView({ block: "nearest" });
}

async function toggleServer() {
    if (pending) return;
    pending = true;
    renderBusy();
    showSettingsError(null);
    try {
        const state = running
            ? await invoke<ServerState>("stop_server")
            : await invoke<ServerState>("start_server", {
                  settings: readSettings(),
              });
        renderServer(state);
    } catch (err) {
        showSettingsError(errorText(err));
    } finally {
        pending = false;
        renderBusy();
    }
}

function appendLog(entry: LogLine) {
    if (entry.seq <= lastLogSeq) return;
    lastLogSeq = entry.seq;
    const log = $("log");
    const atBottom =
        log.scrollHeight - log.scrollTop - log.clientHeight < 4;
    const line = document.createElement("div");
    line.textContent = entry.line;
    log.appendChild(line);
    while (log.childElementCount > MAX_LOG_NODES) {
        log.firstElementChild!.remove();
    }
    if (atBottom) log.scrollTop = log.scrollHeight;
}

function renderUpdate(view: UpdateView) {
    update = view;
    const text = $("update_text");
    const progress = $<HTMLProgressElement>("update_progress");
    const action = $<HTMLButtonElement>("update_action");
    let label: string;
    let button: string | null = null;
    let title = "";
    switch (view.state) {
        case "idle":
            label = `Version ${view.current_version} · ${view.install_kind}`;
            break;
        case "checking":
            label = "Checking…";
            break;
        case "up_to_date":
            label = `Weylus ${view.current_version} is up to date`;
            break;
        case "available":
            label = `Weylus ${view.version} is available`;
            button = "Update";
            break;
        case "downloading":
            label = `Downloading ${view.version} — ${view.progress}`;
            break;
        case "installed":
            label = `Updated to ${view.version}`;
            button = "Restart";
            break;
        case "check_failed":
            label = `Couldn't check: ${view.error}`;
            title = view.error ?? "";
            break;
        case "update_failed":
            label = `Update failed: ${view.error}`;
            button = "Retry";
            title = view.error ?? "";
            break;
    }
    text.textContent = label;
    text.title = view.blocker ?? title;

    progress.hidden = view.state !== "downloading";
    if (view.state === "downloading" && view.fraction !== null) {
        progress.value = view.fraction;
    } else {
        progress.removeAttribute("value");
    }

    action.hidden = button === null;
    if (button !== null) action.textContent = button;

    const blocked = view.blocker !== null;
    $<HTMLButtonElement>("update_check").disabled =
        blocked || view.state === "checking" || view.state === "downloading";
    $<HTMLInputElement>("check_updates").disabled = blocked;
}

async function onUpdateAction() {
    if (update === null) return;
    try {
        if (update.state === "installed") {
            await invoke("update_restart");
        } else {
            await invoke("update_apply");
        }
    } catch (err) {
        console.error(err);
    }
}

function areaInput(device: Device, field: string): HTMLInputElement {
    return $<HTMLInputElement>(`area_${device}_${field}`);
}

function fillAreas(areas: CustomInputAreas) {
    for (const device of DEVICES) {
        const rect = areas[device];
        areaInput(device, "on").checked = rect !== null;
        for (const field of ["x", "y", "w", "h"] as const) {
            areaInput(device, field).value =
                rect === null ? "" : String(Math.round(rect[field] * 1000) / 10);
        }
    }
    $("areas_error").hidden = true;
    $("areas_error").textContent = "";
}

function readAreas(): CustomInputAreas {
    const read = (device: Device): Rect | null => {
        if (!areaInput(device, "on").checked) return null;
        const value = (field: string) =>
            areaInput(device, field).valueAsNumber / 100;
        return { x: value("x"), y: value("y"), w: value("w"), h: value("h") };
    };
    return { mouse: read("mouse"), touch: read("touch"), pen: read("pen") };
}

async function submitAreas() {
    const error = $("areas_error");
    try {
        await invoke("submit_input_areas", { areas: readAreas() });
        $<HTMLDialogElement>("areas_dialog").close();
    } catch (err) {
        error.textContent = errorText(err);
        error.hidden = false;
    }
}

async function cancelAreas() {
    try {
        await invoke("cancel_input_areas");
    } catch (err) {
        console.error(err);
    }
}

let allowedClientsTimer: number | undefined;

async function validateAllowedClients() {
    const input = $<HTMLInputElement>("allowed_clients");
    const error = $("allowed_clients_error");
    try {
        await invoke("check_allowed_clients", { text: input.value });
        error.hidden = true;
        error.textContent = "";
        input.removeAttribute("aria-invalid");
    } catch (err) {
        error.textContent = errorText(err);
        error.hidden = false;
        input.setAttribute("aria-invalid", "true");
    }
}

function renderClients(list: ClientInfo[]) {
    const template = $<HTMLTemplateElement>("client_row");
    const rows = list.map((client) => {
        const row = template.content.firstElementChild!.cloneNode(
            true
        ) as HTMLElement;
        const field = (name: string) => row.querySelector(name) as HTMLElement;
        field(".client-ip").textContent = client.ip;
        const since = new Date(client.connected_at_ms);
        const sinceEl = field(".client-since");
        sinceEl.textContent = since.toLocaleTimeString([], {
            hour: "2-digit",
            minute: "2-digit",
        });
        sinceEl.title = since.toLocaleString();
        const v = client.video;
        field(".client-video").textContent =
            v === null
                ? "Waiting for video"
                : `${v.encoder} · ${v.width}×${v.height} · ${Math.round(v.fps)} fps`;
        for (const action of ["disconnect", "block"]) {
            row.querySelector(`button[data-action="${action}"]`)!.addEventListener(
                "click",
                () => {
                    invoke(`${action}_client`, { id: client.id }).catch(
                        console.error
                    );
                }
            );
        }
        return row;
    });
    $("clients_list").replaceChildren(...rows);
    $("clients_empty").hidden = list.length > 0;
    $("clients_count").textContent = String(list.length);
}

const approvalQueue: ApprovalPrompt[] = [];
let approvalTimer: number | undefined;

function closeApproval() {
    window.clearInterval(approvalTimer);
    approvalTimer = undefined;
    $<HTMLDialogElement>("approval_dialog").close();
}

function renderApprovalQueue() {
    const more = $("approval_queue");
    more.textContent = `+${approvalQueue.length - 1} more`;
    more.hidden = approvalQueue.length < 2;
}

function showNextApproval() {
    if (approvalQueue.length === 0) {
        closeApproval();
        return;
    }
    const prompt = approvalQueue[0];
    const dialog = $<HTMLDialogElement>("approval_dialog");
    $("approval_ip").textContent = prompt.ip;
    renderApprovalQueue();
    const expires = Date.now() + prompt.timeout_secs * 1000;
    const tick = () => {
        const left = Math.ceil((expires - Date.now()) / 1000);
        if (left <= 0) {
            dropApproval(prompt.id);
            return;
        }
        $("approval_countdown").textContent = `Closes in ${left} s`;
    };
    window.clearInterval(approvalTimer);
    tick();
    approvalTimer = window.setInterval(tick, 1000);
    if (!dialog.open) dialog.showModal();
}

function dropApproval(id: number) {
    const index = approvalQueue.findIndex((p) => p.id === id);
    if (index < 0) return;
    approvalQueue.splice(index, 1);
    if (index === 0) {
        showNextApproval();
    } else {
        renderApprovalQueue();
    }
}

async function answerApproval(approval: Approval) {
    const prompt = approvalQueue[0];
    if (prompt === undefined) return;
    dropApproval(prompt.id);
    try {
        const allowed = await invoke<string | null>("answer_approval", {
            id: prompt.id,
            approval,
        });
        if (allowed !== null) {
            $<HTMLInputElement>("allowed_clients").value = allowed;
            validateAllowedClients();
        }
    } catch (err) {
        // the request already expired
        console.error(err);
    }
}

async function init() {
    await Promise.all([
        listen<ClientInfo[]>("clients", (e) => renderClients(e.payload)),
        listen<ApprovalPrompt>("approval", (e) => {
            approvalQueue.push(e.payload);
            if (approvalQueue.length === 1) {
                showNextApproval();
            } else {
                renderApprovalQueue();
            }
        }),
        listen<number>("approval-closed", (e) => dropApproval(e.payload)),
        listen<LogLine>("log", (e) => appendLog(e.payload)),
        listen<UpdateView>("update", (e) => renderUpdate(e.payload)),
        listen<ServerState>("server", (e) => renderServer(e.payload)),
        listen<string>("uinput-inaccessible", (e) => {
            $("uinput_text").textContent = e.payload;
            $<HTMLDialogElement>("uinput_dialog").showModal();
        }),
        listen<CustomInputAreas>("input-areas-requested", (e) => {
            fillAreas(e.payload);
            $<HTMLDialogElement>("areas_dialog").showModal();
        }),
    ]);

    const [settings, server, view, logs, clients] = await Promise.all([
        invoke<Settings>("settings"),
        invoke<ServerState>("server_state"),
        invoke<UpdateView>("update_state"),
        invoke<LogLine[]>("logs"),
        invoke<ClientInfo[] | null>("clients"),
    ]);
    showSettings(settings);
    if (settings.allowed_clients !== "") validateAllowedClients();
    renderServer(server);
    renderUpdate(view);
    renderClients(clients ?? []);
    for (const entry of logs) appendLog(entry);
}

$("toggle_server").addEventListener("click", toggleServer);
$("copy_url").addEventListener("click", () => {
    navigator.clipboard.writeText($("connect_url").textContent ?? "").catch(() => {});
});
$("update_check").addEventListener("click", () => {
    invoke("update_check").catch(console.error);
});
$("update_action").addEventListener("click", onUpdateAction);
$("check_updates").addEventListener("change", (e) => {
    invoke("set_check_updates", {
        enabled: (e.target as HTMLInputElement).checked,
    }).catch(console.error);
});
$("close_to_tray").addEventListener("change", (e) => {
    invoke("set_close_to_tray", {
        enabled: (e.target as HTMLInputElement).checked,
    }).catch(console.error);
});
$("allowed_clients").addEventListener("input", () => {
    window.clearTimeout(allowedClientsTimer);
    allowedClientsTimer = window.setTimeout(validateAllowedClients, 250);
});
$("approval_once").addEventListener("click", () => answerApproval("once"));
$("approval_always").addEventListener("click", () => answerApproval("always"));
$("approval_deny").addEventListener("click", () => answerApproval("deny"));
$("approval_dialog").addEventListener("cancel", (e) => {
    // Esc answers "deny" instead of closing the dialog behind the queue's back
    e.preventDefault();
    answerApproval("deny");
});
$("source_link").addEventListener("click", () => {
    invoke("open_source_link").catch(console.error);
});
$("uinput_close").addEventListener("click", () => {
    $<HTMLDialogElement>("uinput_dialog").close();
});
$("areas_submit").addEventListener("click", submitAreas);
$("areas_cancel").addEventListener("click", () => {
    $<HTMLDialogElement>("areas_dialog").close();
    cancelAreas();
});
$("areas_dialog").addEventListener("cancel", cancelAreas);

init().catch((err) => {
    console.error(err);
    showSettingsError(errorText(err));
});
