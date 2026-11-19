const { invoke } = window.__TAURI__.core;

const el = (id) => document.getElementById(id);
let wsDirty = false;
let lastThemesKey = "";

el("ws-path").addEventListener("input", () => {
    wsDirty = true;
});

function showToast(msg) {
    const t = el("toast");
    t.textContent = msg;
    t.classList.add("show");
    setTimeout(() => t.classList.remove("show"), 2600);
}

function renderState(s) {
    if (!wsDirty && document.activeElement !== el("ws-path")) {
        el("ws-path").value = s.workspace_path || "";
    }
    el("dev-mode-toggle").checked = !!s.dev_mode;
    el("server-url").textContent = s.url;

    const hint = el("ws-hint");
    if (!s.workspace_valid) {
        hint.textContent = "CRust 폴더를 지정해 주세요 (templates 폴더가 있는 경로).";
        hint.style.color = "#fca5a5";
    } else if (s.has_binary) {
        hint.textContent = "준비 완료! 실행 바이너리가 감지되었습니다.";
        hint.style.color = "#6ee7b7";
    } else {
        hint.textContent = "Cargo 프로젝트 감지됨 (시작 시 cargo run 으로 실행됩니다).";
        hint.style.color = "#93c5fd";
    }

    const dot = el("status-dot");
    const stText = el("status-text");
    if (s.running) {
        dot.classList.add("on");
        stText.textContent = `실행 중 (:${s.port})`;
    } else {
        dot.classList.remove("on");
        stText.textContent = "중지됨";
    }

    el("btn-start").disabled = !s.workspace_valid || s.running;
    el("btn-stop").disabled = !s.running;
    el("btn-open-home").disabled = !s.running;
    el("btn-open-admin").disabled = !s.running;
    el("btn-open-themes-page").disabled = !s.running;

    const logEl = el("log-output");
    if (s.logs && s.logs.length > 0) {
        const atBottom = logEl.scrollTop + logEl.clientHeight >= logEl.scrollHeight - 20;
        logEl.textContent = s.logs.join("\n");
        if (atBottom) logEl.scrollTop = logEl.scrollHeight;
    }

    renderThemes(s.themes || [], s.running);
}

function renderThemes(themes, running) {
    const key = JSON.stringify(themes.map((t) => t.slug)) + "|" + running;
    if (key === lastThemesKey) return;
    lastThemesKey = key;

    el("theme-count").textContent = `${themes.length}개`;
    const list = el("theme-list");
    list.innerHTML = "";

    themes.forEach((t) => {
        const item = document.createElement("div");
        item.className = "theme-item";
        item.innerHTML = `
            <div class="theme-meta">
                <strong>${t.name} <code>(${t.slug})</code></strong>
                <span>v${t.version} · ${t.author}</span>
            </div>
            <div class="theme-actions">
                <button class="btn btn-sm" data-act="preview" ${running ? "" : "disabled"}>미리보기</button>
                <button class="btn btn-sm" data-act="folder">폴더</button>
                <button class="btn btn-sm btn-accent" data-act="zip">ZIP 내보내기</button>
            </div>
        `;
        item.querySelector('[data-act="preview"]').onclick = () =>
            invoke("open_in_browser", { suffix: `/?preview_theme=${encodeURIComponent(t.slug)}` });
        item.querySelector('[data-act="folder"]').onclick = () =>
            invoke("open_folder", { subpath: `templates/themes/${t.slug}` }).catch((e) => showToast(e));
        item.querySelector('[data-act="zip"]').onclick = async () => {
            try {
                const p = await invoke("export_theme_zip", { slug: t.slug });
                showToast(`ZIP 생성 완료: ${p}`);
            } catch (e) {
                showToast(String(e));
            }
        };
        list.appendChild(item);
    });
}

async function refresh() {
    try {
        const s = await invoke("get_studio_state");
        renderState(s);
    } catch (e) {
        console.error(e);
    }
}

el("btn-save-ws").onclick = async () => {
    try {
        const s = await invoke("set_workspace_path", { path: el("ws-path").value });
        wsDirty = false;
        renderState(s);
        showToast("작업 폴더가 저장되었습니다.");
    } catch (e) {
        showToast(String(e));
    }
};

el("btn-install-crust").onclick = async () => {
    const targetDir = el("ws-path").value.trim();
    if (!targetDir) {
        showToast("설치할 폴더 경로를 먼저 입력해 주세요 (예: C:\\CRust)");
        return;
    }
    const btn = el("btn-install-crust");
    btn.disabled = true;
    btn.textContent = "설치 중...";
    showToast("GitHub에서 CRust를 다운로드하고 있습니다...");
    try {
        const s = await invoke("install_crust", { targetDir });
        wsDirty = false;
        renderState(s);
        showToast("CRust 설치 완료! 이제 서버 시작을 누르세요.");
    } catch (e) {
        showToast(String(e));
    } finally {
        btn.disabled = false;
        btn.textContent = "🚀 새로 설치";
    }
};

el("btn-open-ws").onclick = () =>
    invoke("open_folder", { subpath: "" }).catch((e) => showToast(String(e)));

el("dev-mode-toggle").onchange = async (e) => {
    try {
        const s = await invoke("toggle_dev_mode", { enabled: e.target.checked });
        renderState(s);
        showToast(`dev_mode = ${e.target.checked} 저장됨`);
    } catch (e) {
        showToast(String(e));
    }
};

el("btn-start").onclick = async () => {
    try {
        const s = await invoke("start_server", { openBrowser: true });
        renderState(s);
        showToast("CRust 서버를 시작했습니다.");
    } catch (e) {
        showToast(String(e));
    }
};

el("btn-stop").onclick = async () => {
    try {
        const s = await invoke("stop_server");
        renderState(s);
        showToast("서버가 중지되었습니다.");
    } catch (e) {
        showToast(String(e));
    }
};

el("btn-open-home").onclick = () => invoke("open_in_browser", { suffix: "/" });
el("btn-open-admin").onclick = () => invoke("open_in_browser", { suffix: "/admin" });
el("btn-open-themes-page").onclick = () => invoke("open_in_browser", { suffix: "/admin/themes" });
el("btn-open-themes-dir").onclick = () =>
    invoke("open_folder", { subpath: "templates/themes" }).catch((e) => showToast(String(e)));

el("btn-create-theme").onclick = async () => {
    const slug = el("new-slug").value.trim();
    const displayName = el("new-name").value.trim();
    const author = el("new-author").value.trim();
    try {
        const s = await invoke("create_theme", { slug, displayName, author });
        el("new-slug").value = "";
        el("new-name").value = "";
        renderState(s);
        showToast(`새 테마 '${slug}' 생성 완료!`);
    } catch (e) {
        showToast(String(e));
    }
};

refresh();
setInterval(refresh, 1500);
