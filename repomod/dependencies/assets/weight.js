"use strict";
(() => {
    "use strict";
    const LANGS = ["npm", "cargo", "pypi", "go", "other"];
    const SHORT = {
        npm: "TS/JS",
        cargo: "Rust",
        pypi: "Python",
        go: "Go",
        other: "Other",
    };
    const UNITS = ["B", "kB", "MB", "GB", "TB"];
    function formatBytes(value) {
        if (value < 1000)
            return value + " B";
        let amount = value;
        let index = 0;
        while (amount >= 1000 && index < UNITS.length - 1) {
            amount /= 1000;
            index += 1;
        }
        let rounded = amount < 10 ? Math.floor(amount * 10 + 0.5) / 10 : Math.floor(amount + 0.5);
        if (rounded >= 1000 && index < UNITS.length - 1) {
            rounded = Math.floor((rounded / 1000) * 10 + 0.5) / 10;
            index += 1;
        }
        const unit = UNITS[index] ?? "TB";
        return (rounded < 10 ? rounded.toFixed(1) : rounded.toFixed(0)) + " " + unit;
    }
    const PLATFORMS = ["darwin-arm64", "linux-x64-musl", "linux-arm64-musl", "all"];
    const DEFAULT_PLATFORM = "darwin-arm64";
    const SIZED = ["npm", "cargo", "pypi"];
    function isLang(value) {
        return LANGS.includes(value);
    }
    function isPlatform(value) {
        return PLATFORMS.includes(value);
    }
    function variantKey(view) {
        return view.platform + (view.dev ? "-dev" : "-run");
    }
    function parseHash(hash) {
        const params = new URLSearchParams(hash.replace(/^#/, ""));
        const wanted = (params.get("lang") ?? "").split(",").filter(isLang);
        const langs = LANGS.filter((lang) => wanted.includes(lang));
        const platform = params.get("platform") ?? "";
        return {
            langs: langs.length > 0 ? langs : [...LANGS],
            platform: isPlatform(platform) ? platform : DEFAULT_PLATFORM,
            dev: params.get("dev") === "1",
            repo: params.get("repo"),
        };
    }
    function formatHash(view) {
        const parts = [];
        if (view.langs.length !== LANGS.length)
            parts.push("lang=" + view.langs.join(","));
        if (view.platform !== DEFAULT_PLATFORM)
            parts.push("platform=" + view.platform);
        if (view.dev)
            parts.push("dev=1");
        if (view.repo !== null)
            parts.push("repo=" + encodeURIComponent(view.repo).replace(/%2F/g, "/"));
        return parts.join("&");
    }
    function summarize(row, view) {
        const sum = { bytes: 0, packages: 0, unknown: 0 };
        const variant = row.totals[variantKey(view)];
        if (variant === undefined)
            return sum;
        for (const lang of view.langs) {
            const part = variant[lang];
            sum.bytes += part.bytes;
            sum.packages += part.packages;
            sum.unknown += part.unknown;
        }
        return sum;
    }
    function order(rows, view) {
        return rows
            .map((row) => ({ repo: row.repo, sum: summarize(row, view) }))
            .sort((left, right) => {
            if (left.sum.bytes !== right.sum.bytes)
                return right.sum.bytes - left.sum.bytes;
            if (left.sum.packages !== right.sum.packages)
                return right.sum.packages - left.sum.packages;
            const a = left.repo.toLowerCase();
            const b = right.repo.toLowerCase();
            return a < b ? -1 : a > b ? 1 : 0;
        })
            .map((entry) => entry.repo);
    }
    function valueText(sum) {
        if (sum.bytes > 0)
            return formatBytes(sum.bytes);
        return sum.packages > 0 ? "size unknown" : "none";
    }
    function noteText(sum) {
        if (sum.packages === 0)
            return "no packages";
        const count = sum.packages + (sum.packages === 1 ? " package" : " packages");
        return sum.unknown > 0 ? count + " · " + sum.unknown + " size unknown" : count;
    }
    function tipText(row, view) {
        const sum = summarize(row, view);
        const variant = row.totals[variantKey(view)];
        const split = view.langs
            .filter((lang) => (variant?.[lang].bytes ?? 0) > 0)
            .map((lang) => SHORT[lang] + " " + formatBytes(variant?.[lang].bytes ?? 0));
        const detail = split.length > 1 ? " (" + split.join(", ") + ")" : "";
        return row.repo + ": " + valueText(sum) + detail + ", " + noteText(sum);
    }
    function emptyText(rows, view, names) {
        const empty = rows
            .filter((row) => summarize(row, view).packages === 0)
            .map((row) => row.repo)
            .sort((left, right) => {
            const a = left.toLowerCase();
            const b = right.toLowerCase();
            return a < b ? -1 : a > b ? 1 : 0;
        });
        if (empty.length === 0)
            return "";
        const what = view.langs.length === LANGS.length ? "third-party" : view.langs.map((lang) => SHORT[lang]).join(" or ");
        const count = empty.length + (empty.length === 1 ? " repository has" : " repositories have") + " no " + what + " dependencies";
        const more = empty.length > names ? " and " + (empty.length - names) + " more" : "";
        return count + ": " + empty.slice(0, names).join(", ") + more + ".";
    }
    function cellText(lang, sum) {
        if (sum.packages === 0)
            return ["\u2014", ""];
        if (SIZED.includes(lang) && sum.bytes > 0)
            return [formatBytes(sum.bytes), String(sum.packages)];
        return [String(sum.packages), "no size"];
    }
    const ZERO = { bytes: 0, packages: 0, unknown: 0 };
    function plural(count, word) {
        return count + " " + word + (count === 1 ? "" : "s");
    }
    function depNote(frees, shared) {
        const count = plural(frees.packages, "package") + " only via it";
        if (shared.packages === 0)
            return count;
        const amount = shared.bytes > 0 ? formatBytes(shared.bytes) : plural(shared.packages, "package");
        return count + " · " + amount + " shared";
    }
    function depTip(label, repo, frees, shared) {
        return label + " in " + repo + ": removing it frees " + valueText(frees) + ", " + depNote(frees, shared);
    }
    function compareText(left, right) {
        const a = left.toLowerCase();
        const b = right.toLowerCase();
        return a < b ? -1 : a > b ? 1 : 0;
    }
    function depOrder(deps, view) {
        const key = variantKey(view);
        return deps
            .filter((dep) => view.langs.includes(dep.lang) && (dep.frees[key] ?? ZERO).packages > 0)
            .map((dep) => ({ dep, frees: dep.frees[key] ?? ZERO, shared: dep.shared[key] ?? ZERO }))
            .sort((left, right) => {
            if (left.frees.bytes !== right.frees.bytes)
                return right.frees.bytes - left.frees.bytes;
            if (left.frees.packages !== right.frees.packages)
                return right.frees.packages - left.frees.packages;
            if (left.shared.bytes !== right.shared.bytes)
                return right.shared.bytes - left.shared.bytes;
            return compareText(left.dep.name, right.dep.name) || compareText(left.dep.repo, right.dep.repo);
        })
            .map((entry) => entry.dep.id);
    }
    function depCount(shown, total) {
        if (total === 0)
            return "No direct dependency in the selection brings in third-party packages.";
        return shown < total ? "Top " + shown + " of " + total + " direct dependencies." : total + " direct dependencies.";
    }
    const handle = {
        LANGS,
        formatBytes,
        parseHash,
        formatHash,
        summarize,
        order,
        valueText,
        noteText,
        tipText,
        emptyText,
        cellText,
        depNote,
        depTip,
        depOrder,
        depCount,
    };
    window.depsWeight = handle;
    const found = document.querySelector("[data-weight]");
    if (found === null)
        return;
    const root = found;
    function number(element, name) {
        const value = Number(element.getAttribute(name));
        return Number.isFinite(value) ? value : 0;
    }
    function readRow(element) {
        const totals = {};
        for (const platform of PLATFORMS) {
            for (const scope of ["run", "dev"]) {
                const variant = {};
                for (const lang of LANGS) {
                    const [bytes, packages, unknown] = (element.getAttribute("data-t-" + platform + "-" + scope + "-" + lang) ?? "0,0,0")
                        .split(",")
                        .map(Number);
                    variant[lang] = { bytes: bytes ?? 0, packages: packages ?? 0, unknown: unknown ?? 0 };
                }
                totals[platform + "-" + scope] = variant;
            }
        }
        return { repo: element.getAttribute("data-repo") ?? "", totals };
    }
    const list = root.querySelector("[data-weight-list]");
    const emptyLine = root.querySelector("[data-empty-line]");
    const rowElements = new Map();
    const rows = [];
    for (const element of root.querySelectorAll("[data-weight-row]")) {
        const row = readRow(element);
        rows.push(row);
        rowElements.set(row.repo, element);
    }
    const tableBody = root.querySelector("[data-weight-table]");
    const tableRows = new Map();
    for (const element of root.querySelectorAll("[data-weight-table-row]")) {
        tableRows.set(element.getAttribute("data-repo") ?? "", element);
    }
    function totals(element, prefix) {
        const found = {};
        for (const platform of PLATFORMS) {
            for (const scope of ["run", "dev"]) {
                const [bytes, packages, unknown] = (element.getAttribute(prefix + platform + "-" + scope) ?? "0,0,0")
                    .split(",")
                    .map(Number);
                found[platform + "-" + scope] = { bytes: bytes ?? 0, packages: packages ?? 0, unknown: unknown ?? 0 };
            }
        }
        return found;
    }
    function readDeps(container) {
        const found = new Map();
        if (container === null)
            return found;
        for (const element of container.querySelectorAll("[data-dep]")) {
            const lang = element.getAttribute("data-lang") ?? "";
            if (!isLang(lang))
                continue;
            const id = String(found.size);
            const dep = {
                id,
                lang,
                name: element.getAttribute("data-name") ?? "",
                repo: element.getAttribute("data-repo") ?? "",
                frees: totals(element, "data-f-"),
                shared: totals(element, "data-s-"),
            };
            found.set(id, [dep, element]);
        }
        return found;
    }
    const depList = root.querySelector("[data-dep-list]");
    const depCaption = root.querySelector("[data-dep-count]");
    const depLimit = number(root, "data-dep-limit");
    const fleetDeps = readDeps(depList);
    const repoDeps = Array.from(root.querySelectorAll("[data-deps]")).map((body) => ({ body, deps: readDeps(body) }));
    const langToggles = Array.from(root.querySelectorAll("[data-lang-toggle]"));
    const platformToggles = Array.from(root.querySelectorAll("[data-platform-toggle]"));
    const devToggle = root.querySelector("[data-dev-toggle]");
    const limit = number(root, "data-limit");
    const names = number(root, "data-names");
    let view = parseHash(window.location.hash);
    function renderRow(element, row, scale) {
        const sum = summarize(row, view);
        element.hidden = sum.packages === 0;
        const summary = element.querySelector("[data-summary]");
        summary?.setAttribute("data-tip", tipText(row, view));
        const fill = element.querySelector("[data-fill]");
        if (fill !== null) {
            fill.style.setProperty("--r", (scale > 0 ? sum.bytes / scale : 0).toFixed(4));
            fill.hidden = sum.bytes === 0;
            const variant = row.totals[variantKey(view)];
            for (const segment of Array.from(fill.querySelectorAll("[data-seg]"))) {
                const lang = segment.getAttribute("data-seg") ?? "";
                const bytes = isLang(lang) && view.langs.includes(lang) ? (variant?.[lang].bytes ?? 0) : 0;
                segment.style.flexGrow = String(bytes);
                segment.hidden = bytes === 0;
            }
        }
        const value = element.querySelector("[data-value]");
        if (value !== null) {
            value.textContent = valueText(sum);
            value.classList.toggle("weight-none", sum.bytes === 0);
        }
        const note = element.querySelector("[data-note]");
        if (note !== null)
            note.textContent = noteText(sum);
        const body = element.querySelector("[data-packages]");
        if (body === null)
            return;
        const items = Array.from(body.querySelectorAll("tr[data-lang]"));
        const shown = items
            .filter((item) => view.langs.some((lang) => lang === item.getAttribute("data-lang")) &&
            (item.getAttribute("data-platforms") ?? "").split(" ").includes(view.platform) &&
            (view.dev || item.getAttribute("data-dev") !== "1"))
            .sort((left, right) => number(right, "data-bytes") - number(left, "data-bytes"));
        const visible = new Set(shown.slice(0, limit > 0 ? limit : shown.length));
        for (const item of shown)
            body.appendChild(item);
        for (const item of items)
            item.hidden = !visible.has(item);
        const caption = element.querySelector("[data-count]");
        if (caption !== null) {
            caption.textContent =
                visible.size === 0
                    ? "The selected languages report no package sizes."
                    : "Heaviest " + visible.size + " of " + sum.packages + " packages" + (sum.unknown > 0 ? ", " + sum.unknown + " without a size" : "") + ".";
        }
    }
    function renderTable(element, row) {
        const sum = summarize(row, view);
        const variant = row.totals[variantKey(view)];
        for (const cell of element.querySelectorAll("[data-cell]")) {
            const lang = cell.getAttribute("data-cell") ?? "";
            if (!isLang(lang) || variant === undefined)
                continue;
            const [text, note] = cellText(lang, variant[lang]);
            cell.textContent = text;
            if (note !== "") {
                const span = document.createElement("span");
                span.className = "cell-note";
                span.textContent = note;
                cell.append(" ", span);
            }
        }
        const bytes = element.querySelector("[data-total-bytes]");
        if (bytes !== null)
            bytes.textContent = valueText(sum);
        const packages = element.querySelector("[data-total-packages]");
        if (packages !== null)
            packages.textContent = String(sum.packages);
        const unknown = element.querySelector("[data-total-unknown]");
        if (unknown !== null)
            unknown.textContent = String(sum.unknown);
    }
    function renderDeps() {
        const key = variantKey(view);
        const sorted = depOrder(Array.from(fleetDeps.values(), ([dep]) => dep), view);
        const visible = new Set(sorted.slice(0, depLimit > 0 ? depLimit : sorted.length));
        const scale = Math.max(0, ...Array.from(visible, (id) => {
            const dep = fleetDeps.get(id)?.[0];
            return dep === undefined ? 0 : (dep.frees[key] ?? ZERO).bytes + (dep.shared[key] ?? ZERO).bytes;
        }));
        for (const id of sorted) {
            const entry = fleetDeps.get(id);
            if (entry !== undefined)
                depList?.appendChild(entry[1]);
        }
        for (const [id, [dep, element]] of fleetDeps) {
            element.hidden = !visible.has(id);
            if (element.hidden)
                continue;
            const frees = dep.frees[key] ?? ZERO;
            const shared = dep.shared[key] ?? ZERO;
            const drawn = frees.bytes + shared.bytes;
            element.querySelector("[data-tip]")?.setAttribute("data-tip", depTip(element.getAttribute("data-label") ?? "", element.getAttribute("data-repo") ?? "", frees, shared));
            const fill = element.querySelector("[data-fill]");
            if (fill !== null) {
                fill.style.setProperty("--r", (scale > 0 ? drawn / scale : 0).toFixed(4));
                fill.hidden = drawn === 0;
            }
            for (const [selector, value] of [["[data-frees]", frees.bytes], ["[data-shared]", shared.bytes]]) {
                const segment = element.querySelector(selector);
                if (segment === null)
                    continue;
                segment.style.flexGrow = String(value);
                segment.hidden = value === 0;
            }
            const value = element.querySelector("[data-value]");
            if (value !== null) {
                value.textContent = valueText(frees);
                value.classList.toggle("weight-none", frees.bytes === 0);
            }
            const note = element.querySelector("[data-dep-note]");
            if (note !== null)
                note.textContent = depNote(frees, shared);
        }
        if (depCaption !== null)
            depCaption.textContent = depCount(visible.size, sorted.length);
        for (const { body, deps } of repoDeps) {
            const order = depOrder(Array.from(deps.values(), ([dep]) => dep), view);
            const shown = new Set(order.slice(0, limit > 0 ? limit : order.length));
            for (const id of order) {
                const entry = deps.get(id);
                if (entry !== undefined)
                    body.appendChild(entry[1]);
            }
            for (const [id, [dep, element]] of deps) {
                element.hidden = !shown.has(id);
                const frees = dep.frees[key] ?? ZERO;
                const text = element.querySelector("[data-frees-text]");
                if (text !== null)
                    text.textContent = valueText(frees);
                const note = element.querySelector("[data-dep-note]");
                if (note !== null)
                    note.textContent = depNote(frees, dep.shared[key] ?? ZERO);
            }
        }
    }
    function render() {
        const selected = view.langs;
        for (const toggle of langToggles) {
            const lang = toggle.getAttribute("data-lang-toggle") ?? "";
            toggle.setAttribute("aria-pressed", String(selected.some((item) => item === lang)));
        }
        for (const toggle of platformToggles) {
            toggle.setAttribute("aria-pressed", String(toggle.getAttribute("data-platform-toggle") === view.platform));
        }
        devToggle?.setAttribute("aria-pressed", String(view.dev));
        for (const element of root.querySelectorAll("[data-lang-show]")) {
            const lang = element.getAttribute("data-lang-show") ?? "";
            element.hidden = !selected.some((item) => item === lang);
        }
        const scale = Math.max(0, ...rows.map((row) => summarize(row, view).bytes));
        const sorted = order(rows, view);
        for (const row of rows) {
            const element = rowElements.get(row.repo);
            if (element !== undefined)
                renderRow(element, row, scale);
            const tableRow = tableRows.get(row.repo);
            if (tableRow !== undefined)
                renderTable(tableRow, row);
        }
        for (const repo of sorted) {
            const element = rowElements.get(repo);
            if (element !== undefined)
                list?.appendChild(element);
            const tableRow = tableRows.get(repo);
            if (tableRow !== undefined)
                tableBody?.appendChild(tableRow);
        }
        renderDeps();
        if (emptyLine !== null) {
            const text = emptyText(rows, view, names);
            emptyLine.textContent = text;
            emptyLine.hidden = text === "";
        }
    }
    function reveal() {
        for (const [repo, element] of rowElements)
            element.classList.toggle("selected", repo === view.repo);
        if (view.repo === null)
            return;
        const target = rowElements.get(view.repo);
        if (target === undefined)
            return;
        const details = target.querySelector("details");
        if (details !== null)
            details.open = true;
        target.scrollIntoView({ block: "start" });
    }
    function change(next) {
        view = { ...view, ...next };
        const hash = formatHash(view);
        window.history.replaceState(null, "", hash === "" ? window.location.pathname + window.location.search : "#" + hash);
        render();
    }
    for (const toggle of langToggles) {
        toggle.addEventListener("click", () => {
            const lang = toggle.getAttribute("data-lang-toggle") ?? "";
            if (!isLang(lang))
                return;
            const on = view.langs.includes(lang);
            if (on && view.langs.length === 1)
                return;
            change({ langs: LANGS.filter((item) => (item === lang ? !on : view.langs.includes(item))) });
        });
    }
    for (const toggle of platformToggles) {
        toggle.addEventListener("click", () => {
            const platform = toggle.getAttribute("data-platform-toggle") ?? "";
            if (isPlatform(platform))
                change({ platform });
        });
    }
    devToggle?.addEventListener("click", () => change({ dev: !view.dev }));
    window.addEventListener("hashchange", () => {
        view = parseHash(window.location.hash);
        render();
        reveal();
    });
    const controls = root.querySelector("[data-weight-controls]");
    if (controls !== null)
        controls.hidden = false;
    render();
    reveal();
})();
